class AppError(Exception):
    code = 0
class ValidationError(AppError):
    code = 422
class AuthError(AppError):
    code = 401
class LockedError(AuthError):
    code = 423
def handle(exc):
    try:
        raise exc
    except LockedError as e:
        return ("locked", e.code)
    except AuthError as e:
        return ("auth", e.code)
    except AppError as e:
        return ("app", e.code)
    except Exception:
        return ("generic", None)
print(handle(LockedError("l")))
print(handle(AuthError("a")))
print(handle(ValidationError("v")))
print(handle(ValueError("x")))
for cls in [AppError, ValidationError, AuthError, LockedError]:
    print(cls.__name__, cls.code, issubclass(cls, AppError), issubclass(cls, Exception))
def reraise_as(exc, wrapper):
    try:
        raise exc
    except Exception as e:
        raise wrapper(str(e)) from e
try:
    reraise_as(ValueError("v"), AuthError)
except AuthError as e:
    print(type(e).__name__, e.args[0], type(e.__cause__).__name__)
print(isinstance(AuthError(), AppError), isinstance(AppError(), AuthError))
