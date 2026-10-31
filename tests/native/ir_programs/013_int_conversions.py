print(int("42"), int("-7"), int("+9"))
print(int("  12  "))
print(int("ff", 16), int("777", 8), int("101", 2), int("z", 36))
print(int(3.9), int(-3.9), int(True))
print(float(7), float("2.5"))
print(int(), float())
print(round(3.5), round(2.5), round(-2.5), round(2.675, 2))
print(round(12345, -2))
try:
    int("12x")
except ValueError as e:
    print("ValueError", e)
