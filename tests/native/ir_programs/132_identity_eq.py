a = None
print(a is None, a is not None, a == None)
print(True is True, False is not True)
x = ...
print(x is Ellipsis, x == ...)
l1 = [1]
l2 = [1]
print(l1 == l2, l1 is l2, l1 is l1)
t1 = (1, 2)
t2 = (1, 2)
print(t1 == t2, t1 is t2 if False else "skip")
s = "abc"
s2 = s
print(s == "abc", s is s2)
print(1 == 1.0, 1 == True, 0 == False, "1" == 1)
print([] == (), {} == {}, [1] == [1.0])
print((1, 2) < (1, 3), (1, 2) < (1, 2, 0), [1] < [1, 0])
print("ab" < "b", "abc" < "abd", "" < "a")
try:
    1 < "a"
except TypeError as e:
    print("TypeError", e)
try:
    {} < {1: 2}
except TypeError as e:
    print("TypeError", e)
print({1: "a"} == {1: "a"}, (1, [2]) == (1, [2]))
print(None == False, 0 == None, "" == None)
