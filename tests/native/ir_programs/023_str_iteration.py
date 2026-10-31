s = "abc"
letters = [c for c in s]
print(letters)
print(list(reversed(s)))
print(min(s), max(s))
print(sorted("banana"))
print("".join(sorted("hello")))
count = 0
for c in "mississippi":
    if c == "i":
        count += 1
print(count)
print(any(c == "s" for c in s), all(c in "abc" for c in s))
print(next(iter(s)))
