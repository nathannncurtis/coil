a = "one"
b = 2
c = 3.5
print(a + "-" + str(b))
print(" ".join([a, str(b)]))
s = ""
for w in ["x", "y"]:
    s += w
print(s)
parts = []
for i in range(4):
    parts.append(f"[{i}]")
print("".join(parts))
print(("a" "b" "c"), "a" + "b" + "c")
print(str(b) + str(c))
print(f"{a!r}-{b:03d}-{c:+.1f}")
print(f"{a}{b}{c}")
print(f"{a} {{literal}} {b}}}")
print("{}".format(1) if False else "no-format-method")
rows = [f"r{i}" for i in range(3)]
print(",".join(rows))
print(len(f"{a}-{b}"))
big = ("ab" * 3) + a
print(big, big.count("ab"))
