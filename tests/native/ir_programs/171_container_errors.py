xs = []
try:
    xs.pop()
except IndexError as e:
    print("IndexError", e)
d = {}
try:
    d.popitem()
except KeyError as e:
    print("KeyError", e)
try:
    [1].index(9)
except ValueError as e:
    print("ValueError", e)
try:
    "abc".index("z")
except ValueError as e:
    print("ValueError", e)
try:
    [1].remove(2)
except ValueError as e:
    print("ValueError", e)
try:
    int(None)
except TypeError as e:
    print("TypeError", e)
try:
    (1).append(2)
except AttributeError as e:
    print("AttributeError", e)
try:
    a, b = 5
except TypeError as e:
    print("TypeError", e)
try:
    for x in 5:
        pass
except TypeError as e:
    print("TypeError", e)
it = iter([])
try:
    next(it)
except StopIteration:
    print("StopIteration")
class NotIterable:
    pass
try:
    list(NotIterable())
except TypeError as e:
    print("TypeError", e)
