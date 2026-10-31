class Machine:
    def __init__(self):
        self.state = "start"
        self.log = []
    def feed(self, event):
        self.state = {"start": {"go": "mid", "stop": "end"},
                      "mid": {"go": "end", "back": "start"},
                      "end": {}}.get(self.state, {}).get(event, "error")
        self.log.append((event, self.state))
        return self.state
m = Machine()
for ev in ["go", "back", "go", "go", "go"]:
    m.feed(ev)
print(m.log)
def lexer(src):
    tokens = []
    cur = ""
    for ch in src + " ":
        if ch.isalnum():
            cur += ch
        else:
            if cur:
                tokens.append(cur)
                cur = ""
            if ch != " ":
                tokens.append(ch)
    return tokens
print(lexer("a1 + b2 * (c3)"))
def rpn_eval(expr):
    stack = []
    ops = {"+": lambda a, b: a + b, "-": lambda a, b: a - b, "*": lambda a, b: a * b}
    for tok in expr.split():
        if tok in ops:
            b = stack.pop()
            a = stack.pop()
            stack.append(ops[tok](a, b))
        else:
            stack.append(int(tok))
    return stack[0]
print(rpn_eval("3 4 + 2 *"))
class Counter2:
    transitions = 0
    def __init__(self, start=0):
        self.v = start
    def bump(self, by=1):
        Counter2.transitions += 1
        self.v += by
        return self.v
c = Counter2()
print(c.bump(), c.bump(5), Counter2.transitions)
