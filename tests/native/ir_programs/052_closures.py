def counter():
    n = 0
    def bump():
        nonlocal n
        n += 1
        return n
    return bump
c1 = counter()
c2 = counter()
print(c1(), c1(), c2())
def adder(n):
    def add(x):
        return x + n
    return add
print(adder(10)(5), adder(1)(2))
fns = []
for i in range(3):
    def f(i=i):
        return i * 10
    fns.append(f)
print([f() for f in fns])
def outer():
    x = 1
    def middle():
        y = 2
        def inner():
            nonlocal x, y
            x += y
            y += 1
            return (x, y)
        return inner
    return middle()
i = outer()
print(i(), i())
def shared():
    cell = 0
    def read():
        return cell
    return read
print(shared()())
