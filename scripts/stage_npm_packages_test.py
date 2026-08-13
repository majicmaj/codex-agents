import importlib.util
from pathlib import Path
import tempfile
import unittest


SCRIPT_PATH = Path(__file__).with_name("stage_npm_packages.py")
SPEC = importlib.util.spec_from_file_location("stage_npm_packages", SCRIPT_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError(f"Unable to import {SCRIPT_PATH}")
STAGE_NPM_PACKAGES = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(STAGE_NPM_PACKAGES)


class LocalArtifactsAvailableTest(unittest.TestCase):
    def test_requires_every_native_target_archive(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            artifacts_dir = Path(temp_dir)
            for target in STAGE_NPM_PACKAGES.BINARY_TARGETS:
                target_dir = artifacts_dir / target
                target_dir.mkdir()
                (target_dir / f"codex-package-{target}.tar.gz").touch()

            self.assertTrue(
                STAGE_NPM_PACKAGES.local_artifacts_available(
                    artifacts_dir,
                    [STAGE_NPM_PACKAGES.CODEX_PACKAGE_COMPONENT],
                )
            )

            missing_target = STAGE_NPM_PACKAGES.BINARY_TARGETS[-1]
            (
                artifacts_dir
                / missing_target
                / f"codex-package-{missing_target}.tar.gz"
            ).unlink()
            self.assertFalse(
                STAGE_NPM_PACKAGES.local_artifacts_available(
                    artifacts_dir,
                    [STAGE_NPM_PACKAGES.CODEX_PACKAGE_COMPONENT],
                )
            )


if __name__ == "__main__":
    unittest.main()
