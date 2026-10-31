class Error(Exception):
    def __init__(self, code, msg):
        super().__init__(msg)
        self.code = code
class NotFound(Error):
    pass
class Conflict(Error):
    def __str__(self):
        return "conflict!"
def find(k):
    if k == "missing":
        raise NotFound(404, "no such key")
    return "ok"
try:
    find("missing")
except NotFound as e:
    print(e.code, e.args[0], str(e))
try:
    find("x")
except Error:
    print("never")
def put(k):
    raise Conflict(409, "dup")
try:
    put("k")
except Error as e:
    print(type(e).__name__, e.code)
class WithRepr(Exception):
    def __repr__(self):
        return "<WithRepr>"
try:
    raise WithRepr("m")
except WithRepr as e:
    print(repr(e), e.args)
print(issubclass(NotFound, Error), issubclass(NotFound, Exception))
print(issubclass(Conflict, Error), not issubclass(Error, Conflict))
try:
    raise NotFound(1, "x")
except Exception as e:
    print(isinstance(e, NotFound), isinstance(e, Error), isinstance(e, Exception), isinstance(e, BaseException))
class NoArgs(Exception):
    pass
try:
    raise NoArgs
except NoArgs as e:
    print(repr(e))
