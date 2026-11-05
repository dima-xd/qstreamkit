import struct
import sys

out = sys.argv[1]
inputs = sys.argv[2:]

lines = ["#pragma once", ""]
for path in inputs:
    name = path.rsplit("/", 1)[-1].rsplit("\\", 1)[-1].split(".")[0]
    data = open(path, "rb").read()
    words = struct.unpack("<%dI" % (len(data) // 4), data)
    lines.append("static const unsigned int spv_%s[] = {" % name)
    for i in range(0, len(words), 8):
        chunk = ", ".join("0x%08x" % w for w in words[i:i + 8])
        lines.append("    " + chunk + ",")
    lines.append("};")
    lines.append("")

open(out, "w").write("\n".join(lines))
print("wrote", out)
