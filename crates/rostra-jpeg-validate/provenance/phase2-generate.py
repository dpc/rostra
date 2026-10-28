"""Synthetic non-user JPEG encoder/fixture builder; no entropy decoder."""
import hashlib
from pathlib import Path
import struct

root = Path("fixtures")
root.mkdir(exist_ok=True)
manifest = []
truth = {}

def save(name, data, expected):
    path = root / (name + ".jpg")
    path.write_bytes(data)
    manifest.append(f"{expected} {path}")
    return data

def segment(marker, data):
    return bytes([255, marker]) + struct.pack(">H", len(data) + 2) + data

def packed(bits):
    bits += "1" * (-len(bits) % 8)
    raw = bytes(int(bits[i:i+8], 2) for i in range(0, len(bits), 8))
    return raw.replace(b"\xff", b"\xff\x00")

def dht(cls, symbols, length=1):
    counts = bytes([len(symbols) if i == length else 0 for i in range(1, 17)])
    return bytes([cls << 4]) + counts + bytes(symbols)

def jpeg(w=1, h=1, sampling=None, block="00", ac=None, dc=None):
    sample = sampling or [0x11]
    n = len(sample)
    components = b"".join(bytes([i+1, s, 0]) for i, s in enumerate(sample))
    header = b"\xff\xd8"
    header += segment(0xe0, b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0")
    header += segment(0xdb, b"\0" + b"\x01" * 64)
    header += segment(0xc0, b"\x08" + struct.pack(">HHB", h, w, n) + components)
    header += segment(0xc4, (dc or dht(0, [0])) + (ac or dht(1, [0])))
    header += segment(0xda, bytes([n]) + b"".join(bytes([i+1, 0]) for i in range(n)) + b"\0\x3f\0")
    mh, mv = sample[0] >> 4, sample[0] & 15
    mcus = ((w+8*mh-1)//(8*mh))*((h+8*mv-1)//(8*mv))
    blocks = mcus * sum((s >> 4)*(s & 15) for s in sample)
    result = header + packed(block * blocks) + b"\xff\xd9"
    truth[hashlib.sha256(result).hexdigest()] = len(block) * blocks
    return result

def entropy_start(data):
    pos = data.index(b"\xff\xda")
    return pos + 2 + int.from_bytes(data[pos+2:pos+4], "big")

tiny = save("gray-1x1-lookahead", jpeg(), "Valid")
save("empty-scan", tiny[:entropy_start(tiny)] + b"\xff\xd9", "Invalid")
for layout, samples in [("gray", [0x11]), ("444", [0x11]*3), ("422", [0x21,0x11,0x11]), ("420", [0x22,0x11,0x11])]:
    for w,h in [(3,2), (9,9), (17,15)]:
        save(f"{layout}-{w}x{h}", jpeg(w,h,samples), "Valid")

# Explicit fixed codewords: DC0=0; AC EOB=00, AC 0/1=01.
save("fast-ac", jpeg(block="0"+"011"+"00", ac=dht(1,[0,1],2)), "Valid")
# Nine-bit code 000000000 for EOB exercises the >8-bit Huffman path.
save("slow-eob", jpeg(block="0"+"0"*9, ac=dht(1,[0],9)), "Valid")
# Sixteen-bit EOB code, and a nine-bit nonzero AC magnitude path.
save("slow16-eob", jpeg(block="0"+"0"*16, ac=dht(1,[0],16)), "Valid")
save("slow-ac", jpeg(block="0"+"000000001"+"1"+"000000000", ac=dht(1,[0,1],9)), "Valid")
# DC category11 with all-one magnitude includes stuffed FF; final all-one
# pad fills the second FF byte in a separate AC control below.
save("stuffed-magnitude", jpeg(block="0"*5+"1"*11+"0", dc=dht(0,[11],5)), "Valid")
# DC0=0, AC EOB=00, AC 0/10=01. Large magnitude yields final stuffed FF
# only when block completes at coefficient63, with no EOB required.
save("stuffed-final-ff", jpeg(block="0"+("01"+"1"*10)*63, ac=dht(1,[0,10],2)), "Valid")
save("illegal-eob-run", jpeg(block="00", ac=dht(1,[0x10])), "Invalid")
save("zrl-overrun", jpeg(block="0"+"0"*4, ac=dht(1,[0xf0])), "Invalid")
save("fast-run-overrun", jpeg(block="0"+("01"+"1")*4, ac=dht(1,[0,0xf1],2)), "Invalid")
save("slow-run-overrun", jpeg(block="0"+("000000001"+"1")*4, ac=dht(1,[0,0xf1],9)), "Invalid")
save("exact-zrl-end", jpeg(block="0"+("001"+"1")*15+"010"*3,
                         ac=dht(1,[0,1,0xf0],3)), "Valid")
save("all-ones-tree", jpeg(ac=dht(1,[0,1],1)), "Invalid")
save("empty-dht", jpeg(ac=bytes([0x10])+bytes(16)), "Invalid")
save("invalid-dc-category", jpeg(dc=dht(0,[12])), "Invalid")
save("invalid-ac-category", jpeg(ac=dht(1,[11])), "Invalid")
save("missing-category", jpeg(block="10"), "Invalid")

# Prefix/entropy repair matrices use only small base files. They are not a
# universal claim that all modifications must be invalid.
base_names = [p for p in sorted(root.glob("*.jpg")) if p.name not in
              {"empty-scan.jpg"} and any(line.startswith("Valid ") and str(p) in line for line in manifest)]
for path in base_names:
    data = path.read_bytes()
    start = entropy_start(data)
    consumed = truth[hashlib.sha256(data).hexdigest()]
    remaining = -consumed % 8
    for cut in range(len(data)):
        save(f"prefix-{path.stem}-{cut}", data[:cut], "NotValid")
    for cut in range(start, len(data)-2):
        # Cutting between FF and its stuffing byte creates a marker-fill
        # sequence before EOI, outside the canonical subset.
        save(f"repair-{path.stem}-{cut-start}", data[:cut]+b"\xff\xd9",
             "Unsupported" if cut > start and data[cut-1] == 255 else "Invalid")
    for value in [0, 1, 127, 255]:
        extra = bytes([value]) + (b"\0" if value == 255 else b"")
        save(f"extra-{path.stem}-{value}", data[:-2]+extra+b"\xff\xd9", "Invalid")
    # Flip each bit in the final unstuffed byte. Some coefficient edits are
    # valid alternative images: only padding-aware expectations come later.
    final = len(data)-3
    if data[final] == 0 and data[final-1] == 255: final -= 1
    for bit in range(8):
        changed = data[final] ^ (1 << bit)
        tail = bytes([changed]) + (b"\0" if changed == 255 else b"")
        save(f"final-bit-{path.stem}-{bit}", data[:final]+tail+b"\xff\xd9",
             "Invalid" if bit < remaining else "Observe")
    raw = data[start:-2].replace(b"\xff\0", b"\xff")
    bits = "".join(f"{b:08b}" for b in raw)[:consumed]
    for remove in range(1, min(8, consumed)+1):
        save(f"bitcut-{path.stem}-{remove}", data[:start]+packed(bits[:-remove])+b"\xff\xd9", "Observe")

Path("manifest.txt").write_text("\n".join(manifest)+"\n")
Path("fixture-sha256.txt").write_text("".join(
    f"{hashlib.sha256(p.read_bytes()).hexdigest()}  {p}\n" for p in sorted(root.glob("*.jpg"))))
print(f"fixtures={len(manifest)} total_bytes={sum(p.stat().st_size for p in root.glob('*.jpg'))}")
Path("encoder-bit-truth.txt").write_text("".join(f"{k} {v}\n" for k,v in sorted(truth.items())))
