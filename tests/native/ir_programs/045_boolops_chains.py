def t(v):
    print("eval", v)
    return v
print(t(1) and t(2))
print(t(0) and t(2))
print(t(0) or t(3))
print(t(1) or t(3))
x = 5
print(1 < x < 10, 1 <= x <= 5, 10 > x > 1)
print(1 < 2 == 2)
print("a" < "b" < "c", "a" in "abc" < "z")
print(not True, not 0, not "", not [1])
print(True and False or True)
print(None == False, None == None, None is None)
print([] == [], [] is [])
print((1, 2) == (1, 2))
print({1, 2} == {2, 1})
print({"a": 1} == {"a": 1})
