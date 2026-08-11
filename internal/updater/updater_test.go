package updater

import (
	"context"
	"crypto/sha256"
	"fmt"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
	"time"
)

func TestMaybeUpdateVerifiesAndAtomicallyReplacesExecutable(t *testing.T) {
	newBinary := []byte("new executable")
	digest := fmt.Sprintf("%x", sha256.Sum256(newBinary))
	assetName := fmt.Sprintf("codex-agents_%s_%s", runtime.GOOS, runtime.GOARCH)

	server := httptest.NewServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		switch request.URL.Path {
		case "/repos/owner/repo/releases/latest":
			fmt.Fprintf(writer, `{"tag_name":"v2.0.0","assets":[{"name":%q,"url":%q,"digest":%q},{"name":"SHA256SUMS","url":%q}]}`,
				assetName, serverURL(request)+"/binary", "sha256:"+digest, serverURL(request)+"/sums")
		case "/binary":
			_, _ = writer.Write(newBinary)
		case "/sums":
			fmt.Fprintf(writer, "%s  %s\n", digest, assetName)
		default:
			http.NotFound(writer, request)
		}
	}))
	defer server.Close()

	directory := t.TempDir()
	executable := filepath.Join(directory, "codex-agents")
	if err := os.WriteFile(executable, []byte("old executable"), 0o755); err != nil {
		t.Fatal(err)
	}
	result, err := MaybeUpdate(context.Background(), Options{
		CurrentVersion: "1.0.0", Repository: "owner/repo", Executable: executable,
		APIBase: server.URL, HTTPClient: server.Client(), CacheFile: filepath.Join(directory, "cache.json"),
	})
	if err != nil {
		t.Fatal(err)
	}
	if !result.Updated || result.Version != "2.0.0" || filepath.Base(result.Executable) != filepath.Base(executable) {
		t.Fatalf("unexpected result: %#v", result)
	}
	installed, err := os.ReadFile(executable)
	if err != nil || string(installed) != string(newBinary) {
		t.Fatalf("installed binary = %q, err=%v", installed, err)
	}
}

func TestMaybeUpdateRejectsChecksumMismatch(t *testing.T) {
	assetName := fmt.Sprintf("codex-agents_%s_%s", runtime.GOOS, runtime.GOARCH)
	server := httptest.NewServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		switch request.URL.Path {
		case "/repos/owner/repo/releases/latest":
			fmt.Fprintf(writer, `{"tag_name":"v2.0.0","assets":[{"name":%q,"url":%q},{"name":"SHA256SUMS","url":%q}]}`,
				assetName, serverURL(request)+"/binary", serverURL(request)+"/sums")
		case "/binary":
			_, _ = writer.Write([]byte("tampered"))
		case "/sums":
			fmt.Fprintf(writer, "%064d  %s\n", 0, assetName)
		}
	}))
	defer server.Close()
	executable := filepath.Join(t.TempDir(), "codex-agents")
	if err := os.WriteFile(executable, []byte("original"), 0o755); err != nil {
		t.Fatal(err)
	}
	_, err := MaybeUpdate(context.Background(), Options{
		CurrentVersion: "1.0.0", Repository: "owner/repo", Executable: executable,
		APIBase: server.URL, HTTPClient: server.Client(), CacheFile: filepath.Join(t.TempDir(), "cache.json"),
	})
	if err == nil || !strings.Contains(err.Error(), "checksum mismatch") {
		t.Fatalf("expected checksum error, got %v", err)
	}
	installed, _ := os.ReadFile(executable)
	if string(installed) != "original" {
		t.Fatalf("failed update replaced executable: %q", installed)
	}
}

func TestRecentCheckSkipsNetwork(t *testing.T) {
	directory := t.TempDir()
	cache := filepath.Join(directory, "cache.json")
	now := time.Date(2026, 8, 11, 12, 0, 0, 0, time.UTC)
	if err := writeCache(cache, now); err != nil {
		t.Fatal(err)
	}
	result, err := MaybeUpdate(context.Background(), Options{
		CurrentVersion: "1.0.0", Repository: "owner/repo", APIBase: "http://invalid.invalid",
		CacheFile: cache, Now: func() time.Time { return now.Add(time.Hour) },
	})
	if err != nil || result.Updated {
		t.Fatalf("cached check did not skip network: result=%#v err=%v", result, err)
	}
}

func TestVersionComparison(t *testing.T) {
	if !newer("1.10.0", "1.9.9") || newer("1.9.9", "1.10.0") || newer("dev", "1.0.0") {
		t.Fatal("semantic version comparison is incorrect")
	}
}

func serverURL(request *http.Request) string {
	return "http://" + request.Host
}
