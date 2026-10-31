class AppError(Exception):
    pass
def load(n):
    if n > 2:
        raise AppError("cannot load", n)
    return n * 2
print(load(1))
print(load(5))
