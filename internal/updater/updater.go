package updater

import (
	"bufio"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"time"
)

const (
	defaultRepository = "majicmaj/codex-agents"
	checkInterval     = 24 * time.Hour
	maxChecksumBytes  = 1 << 20
)

type Options struct {
	CurrentVersion string
	Force          bool
	Repository     string
	Executable     string
	APIBase        string
	HTTPClient     *http.Client
	Token          string
	CacheFile      string
	Now            func() time.Time
}

type Result struct {
	Updated    bool
	Version    string
	Executable string
}

type release struct {
	TagName string  `json:"tag_name"`
	Assets  []asset `json:"assets"`
}

type asset struct {
	Name   string `json:"name"`
	URL    string `json:"url"`
	Digest string `json:"digest"`
}

type cacheState struct {
	CheckedAt time.Time `json:"checked_at"`
}

func MaybeUpdate(ctx context.Context, options Options) (Result, error) {
	options = defaults(options)
	if !options.Force && (options.CurrentVersion == "" || options.CurrentVersion == "dev") {
		return Result{}, nil
	}
	if !options.Force && checkedRecently(options.CacheFile, options.Now()) {
		return Result{}, nil
	}
	if options.Token == "" {
		options.Token = githubToken()
	}

	latest, err := latestRelease(ctx, options)
	if err != nil {
		return Result{}, err
	}
	latestVersion := strings.TrimPrefix(latest.TagName, "v")
	if options.CurrentVersion != "dev" && !newer(latestVersion, options.CurrentVersion) {
		_ = writeCache(options.CacheFile, options.Now())
		return Result{Version: latestVersion}, nil
	}

	binaryName := fmt.Sprintf("codex-agents_%s_%s", runtime.GOOS, runtime.GOARCH)
	binaryAsset, ok := findAsset(latest.Assets, binaryName)
	if !ok {
		return Result{}, fmt.Errorf("release %s has no asset for %s/%s", latest.TagName, runtime.GOOS, runtime.GOARCH)
	}
	checksumsAsset, ok := findAsset(latest.Assets, "SHA256SUMS")
	if !ok {
		return Result{}, fmt.Errorf("release %s has no SHA256SUMS", latest.TagName)
	}

	checksums, err := downloadBytes(ctx, options, checksumsAsset.URL, maxChecksumBytes)
	if err != nil {
		return Result{}, fmt.Errorf("download checksums: %w", err)
	}
	expected, err := checksumFor(checksums, binaryName)
	if err != nil {
		return Result{}, err
	}

	executable := options.Executable
	if executable == "" {
		executable, err = os.Executable()
		if err != nil {
			return Result{}, fmt.Errorf("locate executable: %w", err)
		}
	}
	if resolved, resolveErr := filepath.EvalSymlinks(executable); resolveErr == nil {
		executable = resolved
	}
	info, err := os.Stat(executable)
	if err != nil {
		return Result{}, fmt.Errorf("inspect executable: %w", err)
	}
	temporary, err := os.CreateTemp(filepath.Dir(executable), ".codex-agents-update-*")
	if err != nil {
		return Result{}, fmt.Errorf("prepare update beside %s: %w", executable, err)
	}
	temporaryPath := temporary.Name()
	defer os.Remove(temporaryPath)

	hash := sha256.New()
	if err := downloadTo(ctx, options, binaryAsset.URL, io.MultiWriter(temporary, hash)); err != nil {
		temporary.Close()
		return Result{}, fmt.Errorf("download %s: %w", binaryName, err)
	}
	if err := temporary.Sync(); err != nil {
		temporary.Close()
		return Result{}, fmt.Errorf("sync update: %w", err)
	}
	if err := temporary.Close(); err != nil {
		return Result{}, fmt.Errorf("close update: %w", err)
	}
	actual := hex.EncodeToString(hash.Sum(nil))
	if !strings.EqualFold(actual, expected) {
		return Result{}, fmt.Errorf("checksum mismatch for %s", binaryName)
	}
	if binaryAsset.Digest != "" && !strings.EqualFold(binaryAsset.Digest, "sha256:"+actual) {
		return Result{}, fmt.Errorf("GitHub asset digest mismatch for %s", binaryName)
	}
	if err := os.Chmod(temporaryPath, info.Mode().Perm()|0o111); err != nil {
		return Result{}, fmt.Errorf("make update executable: %w", err)
	}
	if err := os.Rename(temporaryPath, executable); err != nil {
		return Result{}, fmt.Errorf("replace %s: %w", executable, err)
	}
	_ = writeCache(options.CacheFile, options.Now())
	return Result{Updated: true, Version: latestVersion, Executable: executable}, nil
}

func defaults(options Options) Options {
	if options.Repository == "" {
		options.Repository = defaultRepository
	}
	if options.APIBase == "" {
		options.APIBase = "https://api.github.com"
	}
	if options.HTTPClient == nil {
		options.HTTPClient = &http.Client{Timeout: 20 * time.Second}
	}
	if options.Now == nil {
		options.Now = time.Now
	}
	if options.CacheFile == "" {
		if cacheDir, err := os.UserCacheDir(); err == nil {
			options.CacheFile = filepath.Join(cacheDir, "codex-agents", "update.json")
		}
	}
	return options
}

func githubToken() string {
	for _, name := range []string{"GH_TOKEN", "GITHUB_TOKEN"} {
		if token := strings.TrimSpace(os.Getenv(name)); token != "" {
			return token
		}
	}
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	output, err := exec.CommandContext(ctx, "gh", "auth", "token").Output()
	if err != nil {
		return ""
	}
	return strings.TrimSpace(string(output))
}

func latestRelease(ctx context.Context, options Options) (release, error) {
	var result release
	url := strings.TrimRight(options.APIBase, "/") + "/repos/" + options.Repository + "/releases/latest"
	body, err := downloadJSON(ctx, options, url, 4<<20)
	if err != nil {
		if strings.Contains(err.Error(), "404") && options.Token == "" {
			return result, errors.New("latest release is private; install GitHub CLI and run `gh auth login`")
		}
		return result, err
	}
	if err := json.Unmarshal(body, &result); err != nil {
		return result, fmt.Errorf("decode latest release: %w", err)
	}
	if result.TagName == "" {
		return result, errors.New("latest release has no tag")
	}
	return result, nil
}

func downloadBytes(ctx context.Context, options Options, url string, limit int64) ([]byte, error) {
	var output strings.Builder
	if err := downloadTo(ctx, options, url, &limitedWriter{writer: &output, remaining: limit}); err != nil {
		return nil, err
	}
	return []byte(output.String()), nil
}

func downloadJSON(ctx context.Context, options Options, url string, limit int64) ([]byte, error) {
	var output strings.Builder
	if err := downloadToAccept(ctx, options, url, "application/vnd.github+json", &limitedWriter{writer: &output, remaining: limit}); err != nil {
		return nil, err
	}
	return []byte(output.String()), nil
}

func downloadTo(ctx context.Context, options Options, url string, destination io.Writer) error {
	return downloadToAccept(ctx, options, url, "application/octet-stream", destination)
}

func downloadToAccept(ctx context.Context, options Options, url, accept string, destination io.Writer) error {
	request, err := http.NewRequestWithContext(ctx, http.MethodGet, url, nil)
	if err != nil {
		return err
	}
	request.Header.Set("Accept", accept)
	request.Header.Set("User-Agent", "codex-agents/"+options.CurrentVersion)
	if options.Token != "" {
		request.Header.Set("Authorization", "Bearer "+options.Token)
	}
	response, err := options.HTTPClient.Do(request)
	if err != nil {
		return err
	}
	defer response.Body.Close()
	if response.StatusCode < 200 || response.StatusCode >= 300 {
		_, _ = io.Copy(io.Discard, io.LimitReader(response.Body, 64<<10))
		return fmt.Errorf("GitHub returned %s", response.Status)
	}
	_, err = io.Copy(destination, response.Body)
	return err
}

type limitedWriter struct {
	writer    io.Writer
	remaining int64
}

func (w *limitedWriter) Write(data []byte) (int, error) {
	if int64(len(data)) > w.remaining {
		return 0, errors.New("download exceeded size limit")
	}
	written, err := w.writer.Write(data)
	w.remaining -= int64(written)
	return written, err
}

func checksumFor(manifest []byte, name string) (string, error) {
	scanner := bufio.NewScanner(strings.NewReader(string(manifest)))
	for scanner.Scan() {
		fields := strings.Fields(scanner.Text())
		if len(fields) == 2 && strings.TrimPrefix(fields[1], "*") == name {
			if len(fields[0]) != sha256.Size*2 {
				break
			}
			if _, err := hex.DecodeString(fields[0]); err == nil {
				return fields[0], nil
			}
		}
	}
	return "", fmt.Errorf("SHA256SUMS has no valid checksum for %s", name)
}

func findAsset(assets []asset, name string) (asset, bool) {
	for _, candidate := range assets {
		if candidate.Name == name {
			return candidate, true
		}
	}
	return asset{}, false
}

func newer(candidate, current string) bool {
	candidateParts, candidateOK := versionParts(candidate)
	currentParts, currentOK := versionParts(current)
	if !candidateOK || !currentOK {
		return false
	}
	for index := range candidateParts {
		if candidateParts[index] != currentParts[index] {
			return candidateParts[index] > currentParts[index]
		}
	}
	return false
}

func versionParts(version string) ([3]int, bool) {
	var result [3]int
	version = strings.TrimPrefix(strings.TrimSpace(version), "v")
	version = strings.SplitN(version, "-", 2)[0]
	parts := strings.Split(version, ".")
	if len(parts) != 3 {
		return result, false
	}
	for index, part := range parts {
		value, err := strconv.Atoi(part)
		if err != nil || value < 0 {
			return result, false
		}
		result[index] = value
	}
	return result, true
}

func checkedRecently(path string, now time.Time) bool {
	if path == "" {
		return false
	}
	data, err := os.ReadFile(path)
	if err != nil {
		return false
	}
	var state cacheState
	return json.Unmarshal(data, &state) == nil && now.Sub(state.CheckedAt) >= 0 && now.Sub(state.CheckedAt) < checkInterval
}

func writeCache(path string, now time.Time) error {
	if path == "" {
		return nil
	}
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return err
	}
	data, err := json.Marshal(cacheState{CheckedAt: now})
	if err != nil {
		return err
	}
	return os.WriteFile(path, data, 0o600)
}
