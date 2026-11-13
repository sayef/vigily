"""The SDK wheel: pure Python, small, and nothing compiled in it."""

import shutil
import subprocess
import zipfile

import pytest
from conftest import ROOT


def test_the_wheel_is_pure_and_small(tmp_path):
    uv = shutil.which("uv")
    if uv is None:
        pytest.skip("uv builds the wheel")
    subprocess.run(
        [uv, "build", "--wheel", "--out-dir", str(tmp_path), str(ROOT)],
        check=True,
        capture_output=True,
    )
    (wheel,) = tmp_path.glob("*.whl")
    assert wheel.name.endswith("-py3-none-any.whl"), wheel.name
    assert wheel.stat().st_size < 200 * 1024, f"{wheel.stat().st_size} bytes"
    with zipfile.ZipFile(wheel) as z:
        names = z.namelist()
        meta = next(n for n in names if n.endswith(".dist-info/METADATA"))
        requires = [
            line
            for line in z.read(meta).decode().splitlines()
            if line.startswith("Requires-Dist:") and "extra ==" not in line
        ]
    assert not [n for n in names if n.endswith((".so", ".pyd", ".dylib", ".dll"))]
    assert "vigily/py.typed" in names
    assert requires == [], "the SDK has no runtime dependencies"


def test_version_is_pep_440():
    import re

    import vigily
    from vigily import _pep440

    # The same form in editable installs as in built wheels (0.1.0b3, not 0.1.0-beta.3).
    assert re.fullmatch(r"\d+\.\d+\.\d+((a|b|rc)\d+)?", vigily.__version__), vigily.__version__
    assert _pep440("0.1.0-beta.3") == "0.1.0b3"
    assert _pep440("1.2.0-rc.1") == "1.2.0rc1"
    assert _pep440("0.2.0-alpha.4") == "0.2.0a4"
    assert _pep440("0.1.0") == "0.1.0"
