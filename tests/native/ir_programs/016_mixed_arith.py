print(1 + 2.5, 2.5 + 1, 7 // 2.0, 7 % 2.0)
print(3 ** 2, 3 ** 0.5 == 3 ** 0.5)
print(2 ** -1)
print(10 ** 20 / 10 ** 19)
print(True + True, True * 5, False - 1)
print(True == 1, False == 0, True + 1.5)
print(isinstance(True, int), issubclass(bool, int))
print(type(True), type(1), type(1.0), type("s"), type(None), type(...))
print((5).bit_length(), (255).bit_length(), (-1).bit_length())
try:
    1.0 / 0
except ZeroDivisionError as e:
    print("ZeroDivisionError", e)
try:
    1 // 0
except ZeroDivisionError as e:
    print("ZeroDivisionError", e)
try:
    1 % 0
except ZeroDivisionError as e:
    print("ZeroDivisionError", e)
