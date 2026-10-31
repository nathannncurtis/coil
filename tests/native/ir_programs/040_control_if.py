def grade(n):
    if n >= 90:
        return "A"
    elif n >= 80:
        return "B"
    elif n >= 70:
        return "C"
    else:
        return "F"
print(grade(95), grade(85), grade(75), grade(10))
x = 5
if x:
    print("truthy")
if not 0:
    print("falsy-not")
if [] or "y":
    print("or-truthy")
if None and 1:
    print("never")
print("a" if x > 3 else "b")
print(0 or "fallback", "" or 0 or "last", 1 and "second", None and "never")
