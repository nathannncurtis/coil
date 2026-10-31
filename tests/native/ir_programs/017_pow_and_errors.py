print(pow(2, 10), pow(2, 10, 1000), pow(3, 4, 5))
print(divmod(-13, 4))
print(0 ** 0, 0.0 ** 0)
try:
    divmod(1, 0)
except ZeroDivisionError as e:
    print("ZeroDivisionError", e)
try:
    0.0 ** -1
except ZeroDivisionError as e:
    print("ZeroDivisionError", e)
print((-8) ** (1 / 3) if False else "skip")
print(7.0 % 3, -7.0 % 3, 7.0 % -3, 7 % 3.5)
