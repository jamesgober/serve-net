"""Lists every function and type docs/API.md names that the source lacks.

Run before a release: anything it prints that the document tags for this
version or earlier is a gap. Items tagged for a later version are skipped;
re-exports from net-io and items with their version in a table rather than on
the signature line show up and are read by hand.
"""

import glob
import re
from pathlib import Path

root = Path(__file__).resolve().parent.parent
api = (root / "docs" / "API.md").read_text(encoding="utf-8")
src = "".join(
    Path(f).read_text(encoding="utf-8")
    for f in glob.glob(str(root / "src" / "**" / "*.rs"), recursive=True)
)

missing = {}
for block in re.findall(r"```rust[^\n]*\n(.*?)```", api, re.S):
    for line in block.split("\n"):
        if line.strip().startswith(("#", "//", "let ", "use ")):
            continue
        version = re.search(r"0\.(\d)\.\d", line)
        current = re.search(r'^version = "0\.(\d)', (root / "Cargo.toml").read_text(encoding="utf-8"), re.M)
        if version and current and int(version.group(1)) > int(current.group(1)):
            continue
        fn = re.search(r"\bfn\s+([a-z_0-9]+)", line) if ("pub fn" in line or re.match(r"\s+fn ", line)) else None
        kind = re.search(r"pub (?:struct|enum|trait)\s+([A-Za-z0-9_]+)", line)
        if fn and not re.search(r"fn\s+" + fn.group(1) + r"\b", src):
            missing.setdefault(("fn", fn.group(1)), line.strip()[:110])
        if kind and not re.search(r"(struct|enum|trait|type)\s+" + kind.group(1) + r"\b", src):
            missing.setdefault(("type", kind.group(1)), line.strip()[:110])

for (what, name), line in sorted(missing.items()):
    print(f"{what} {name} :: {line}")
print(f"{len(missing)} to read")
