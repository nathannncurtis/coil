words = "the quick brown fox jumps over the lazy dog".split()
print(sorted(words))
print(sorted(words, key=str.lower))
print(sorted(words, key=len, reverse=True))
pairs = [(2, "b"), (1, "c"), (2, "a"), (1, "b")]
print(sorted(pairs))
print(sorted(pairs, key=lambda t: (t[0], -ord(t[1][0]))))
data = [("x", 3), ("y", 1), ("z", 2)]
data.sort(key=lambda kv: kv[1])
print(data)
nums = [5, -3, 8, -1]
print(sorted(nums), sorted(nums, key=abs))
print(sorted([True, False, True]))
print(sorted(["b", "a"], reverse=True), sorted("hello"))
print(min(words, key=len), max(words))
print(sorted([(1, [2]), (1, [1])]))
class K:
    def __init__(self, k, tag):
        self.k = k
        self.tag = tag
    def __lt__(self, other):
        return self.k < other.k
    def __repr__(self):
        return self.tag
ks = [K(3, "c"), K(1, "a"), K(2, "b")]
print(sorted(ks))
print(sorted(range(5), reverse=True))
