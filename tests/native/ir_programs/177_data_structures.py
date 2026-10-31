class Stack:
    def __init__(self):
        self.items = []
    def push(self, v):
        self.items.append(v)
    def pop(self):
        return self.items.pop()
    def peek(self):
        return self.items[-1]
    def __len__(self):
        return len(self.items)
s = Stack()
s.push(1)
s.push(2)
print(len(s), s.peek(), s.pop(), len(s))
class Node:
    def __init__(self, val, nxt=None):
        self.val = val
        self.nxt = nxt
    def __iter__(self):
        cur = self
        while cur:
            yield cur.val
            cur = cur.nxt
head = Node(1, Node(2, Node(3)))
print(list(head))
def reverse_list(head):
    prev = None
    cur = head
    while cur:
        cur.nxt, prev, cur = prev, cur, cur.nxt
    return prev
print(list(reverse_list(head)))
class Queue:
    def __init__(self):
        self.data = []
    def add(self, v):
        self.data.append(v)
    def take(self):
        return self.data.pop(0)
q = Queue()
q.add("a")
q.add("b")
print(q.take(), q.take())
class Dict:
    def __init__(self):
        self.keys = []
        self.vals = []
    def set(self, k, v):
        if k in self.keys:
            self.vals[self.keys.index(k)] = v
        else:
            self.keys.append(k)
            self.vals.append(v)
    def get(self, k):
        return self.vals[self.keys.index(k)]
dd = Dict()
dd.set("a", 1)
dd.set("a", 2)
dd.set("b", 3)
print(dd.get("a"), dd.get("b"), dd.keys)
class Tree:
    def __init__(self, children=None):
        self.children = children or []
    def depth(self):
        return 1 + max((c.depth() for c in self.children), default=0)
print(Tree([Tree([Tree()]), Tree()]).depth())
