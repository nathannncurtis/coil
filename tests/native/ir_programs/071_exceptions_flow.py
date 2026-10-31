log = []
try:
    log.append("try")
except ValueError:
    log.append("except")
else:
    log.append("else")
finally:
    log.append("finally")
print(log)
log = []
try:
    log.append("try")
    raise ValueError
except ValueError:
    log.append("except")
finally:
    log.append("finally")
print(log)
log = []
try:
    try:
        raise ValueError("inner")
    finally:
        log.append("inner-finally")
except ValueError as e:
    log.append("caught " + str(e))
print(log)
log = []
for i in range(3):
    try:
        if i == 1:
            raise ValueError(i)
        log.append(i)
    except ValueError as e:
        log.append("v" + str(e))
print(log)
def f():
    try:
        return "from-try"
    finally:
        print("finally-on-return")
print(f())
log = []
while True:
    try:
        log.append("loop")
        break
    finally:
        log.append("fin")
print(log)
log = []
try:
    log.append("a")
except ValueError:
    log.append("never")
else:
    log.append("else2")
print(log)
