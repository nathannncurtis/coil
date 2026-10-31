name = "world"
n = 42
print(f"hello {name}")
print(f"{n} = {n:#x} ({n:05d})")
print(f"{name!r}, {n!s}, {'a'!r}")
w = 8
print(f"{n:{w}}|")
print(f"{'nested {n} braces'}")
print(f"{n + 1} and {n * 2}")
print(f"{name.upper()}")
d = {"k": "v"}
print(f"{d['k']}")
print(f"line1\n{n}" == "line1\n42")
print(f"{'':>3}|")
x = 3.5
print(f"{x=}", f"{x = :.1f}")
print(f"{n:>{w // 2}}")
print(f"a{"b" + "c"}d")
