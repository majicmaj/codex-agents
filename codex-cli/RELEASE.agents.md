# Local npm release

This path publishes Apple Silicon plus the root launcher. It does not publish the other native platforms.

1. Choose the next patch version from `npm view codex-agents version` and replace `<version>` below.
2. Run `just fmt` and `just test -p codex-tui` from `codex-rs`.
3. Commit all intended changes, then push:

   ```sh
   git push agents main
   ```

4. Build the Apple Silicon binaries from the pushed commit:

   ```sh
   cargo build --target aarch64-apple-darwin --release --bin codex --bin codex-code-mode-host
   ```

   If upstream `rusty_v8` returns 404, follow `.github/actions/setup-rusty-v8/action.yml`: download the matching archive, binding, and checksum from the `openai/codex` `rusty-v8-v<crate-version>` release, verify the checksum, then rerun Cargo with `RUSTY_V8_ARCHIVE` and `RUSTY_V8_SRC_BINDING_PATH` set.

5. Build the native archive with Python 3.11 or newer. The `.tar.zst` output is not needed for npm.

   ```sh
   bash .github/scripts/build-codex-package-archive.sh \
     --target aarch64-apple-darwin \
     --bundle primary \
     --entrypoint-dir codex-rs/target/aarch64-apple-darwin/release \
     --archive-dir <native-output>
   ```

6. Extract that archive under `<vendor>/aarch64-apple-darwin`, then stage both tarballs:

   ```sh
   python3.11 codex-cli/scripts/build_npm_package.py \
     --package codex-agents-darwin-arm64 \
     --release-version <version> \
     --staging-dir <empty-platform-dir> \
     --pack-output <output>/codex-agents-<version>-darwin-arm64.tgz \
     --vendor-src <vendor>

   python3.11 codex-cli/scripts/build_npm_package.py \
     --package codex-agents \
     --release-version <version> \
     --staging-dir <empty-root-dir> \
     --pack-output <output>/codex-agents-<version>.tgz
   ```

7. Inspect both `package/package.json` files and smoke-test the staged native `codex --version`.
8. Publish the platform package first, then the root package:

   ```sh
   npm publish <output>/codex-agents-<version>-darwin-arm64.tgz --access public --tag darwin-arm64
   npm publish <output>/codex-agents-<version>.tgz --access public --tag latest
   ```

9. Run publish commands in a TTY. When npm prints its authentication URL, open that exact URL in Canary:

   ```sh
   open -a 'Google Chrome Canary' '<npm-auth-url>'
   ```

   Approve in Canary and keep the publish process running until it prints `+ codex-agents@...`.

10. Verify npm, then create the matching GitHub release:

    ```sh
    npm view codex-agents version dist-tags --json
    npm view codex-agents@<version>-darwin-arm64 version --json
    gh release create v<version> --repo majicmaj/codex-agents --target main \
      --title v<version> --notes '<short release note>'
    ```
