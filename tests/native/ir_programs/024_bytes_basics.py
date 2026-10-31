b = b"hello"
print(b, len(b), b[0], b[1])
print(b + b"!", b * 2)
print(b"hello" == b"hello", b != b"world")
print(b.upper(), b.replace(b"l", b"L"))
print(b.decode("ascii"), "HELLO".encode())
print(bytes([65, 66]), bytes(3))
print(b.find(b"ll"), b.startswith(b"he"), b.split(b"l"))
print(list(b), bytes(list(b)) == b)
try:
    b[0] = 66
except TypeError as e:
    print("TypeError", e)
