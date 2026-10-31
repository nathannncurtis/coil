order = []
def run(tag):
    try:
        order.append(tag + ":try")
        if tag == "raise":
            raise ValueError(tag)
        return tag + "-ret"
    except ValueError:
        order.append(tag + ":except")
        return tag + "-exc-ret"
    finally:
        order.append(tag + ":finally")
print(run("clean"))
print(run("raise"))
print(order)
def nested_finally(n):
    try:
        try:
            try:
                raise ValueError(n)
            finally:
                print("fin3", n)
        finally:
            print("fin2", n)
    except ValueError as e:
        return "caught " + str(e)
print(nested_finally(1), nested_finally(2))
def re_raise():
    try:
        raise ValueError("orig")
    except ValueError:
        try:
            raise TypeError("second")
        finally:
            print("re-raise-fin")
    return "done"
try:
    re_raise()
except TypeError as e:
    print("outer-caught", e)
def breaks_out():
    for i in range(3):
        try:
            if i == 1:
                break
        finally:
            print("loop-fin", i)
    return "broke"
print(breaks_out())
def continues():
    seen = []
    for i in range(3):
        try:
            if i == 1:
                continue
            seen.append(i)
        finally:
            seen.append("f" + str(i))
    return seen
print(continues())
