s = "hello world"
print(s[0], s[-1], s[4])
print(s[0:5], s[6:], s[:5], s[:])
print(s[0:11:2], s[::-1], s[-5:-1])
print(len(s), "world" in s, "x" not in s)
print(s + "!", s * 2, "ab" * 3)
print("a" < "b", "abc" < "abd", "Z" < "a", "abc" == "abc")
print(s.index("o"), s.count("o"), s.find("z"))
try:
    s.find
    s[100]
except IndexError as e:
    print("IndexError", e)
