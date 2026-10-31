data = [1, 2, 3, 4, 5, 6]
if (n := len(data)) > 5:
    print("big", n)
print(n)
i = 0
while (v := data[i]) < 5:
    i += 1
print(i, v)
print([y for x in data if (y := x * 2) > 6])
print(y)
print(w := 3)
