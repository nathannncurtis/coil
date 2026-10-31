total = 0
for i in range(5):
    total += i
print(total)
i = 0
while i < 4:
    i += 1
print(i)
for i in range(10):
    if i == 3:
        continue
    if i == 6:
        break
    print(i, end=" ")
print()
for i in range(3):
    for j in range(3):
        if j == 1:
            break
        print(i, j, end="; ")
print()
n = 0
while True:
    n += 1
    if n >= 3:
        break
print(n)
for i in range(3):
    pass
else:
    print("for-else-ran")
for i in range(3):
    if i == 1:
        break
else:
    print("never")
i = 0
while i < 3:
    i += 1
else:
    print("while-else-ran")
while False:
    pass
else:
    print("while-else-zero")
