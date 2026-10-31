x: int = 5
y: "anything"
z: str
print(x, "y" in globals(), "z" in globals())
x: int = 7
print(x)
lst: list = [1]
lst.append(2)
print(lst)
class Ann:
    a: int = 1
    b: str = "s"
    c: int
print(Ann.a, Ann.b)
i = Ann()
i.c = 3
print(i.c, Ann.a)
def annot_fn(p: int, q: int = 2) -> int:
    r: int = p + q
    return r
print(annot_fn(1))
total: int = 0
for v in range(3):
    total += v
print(total)
d: dict = {}
d["k"] = 1
print(d)
with_val: tuple = (1, 2)
print(with_val)
