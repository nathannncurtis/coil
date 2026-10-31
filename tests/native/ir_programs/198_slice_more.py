xs = list(range(10))
s1 = slice(2)
s2 = slice(1, 8, 2)
print(xs[s1], xs[s2])
print(xs[slice(None, None, -1)])
class SeqLike:
    def __getitem__(self, key):
        if isinstance(key, slice):
            return ("slice", key.start, key.stop, key.step)
        return ("index", key)
sl = SeqLike()
print(sl[1], sl[1:3], sl[::2], sl[10:0:-1])
print(xs[:], xs[2:], xs[:5], xs[::2], xs[::-3])
print(len(xs[3:7]), xs[3:3])
print("abcdefgh"[2:5], "abcdefgh"[::2], "abcdefgh"[::-1])
print((1, 2, 3, 4, 5)[1::2])
sub = [1, 2, 3, 4, 5]
sub[1:3] = [9]
print(sub)
sub[::2] = [0, 0]
print(sub)
b = bytes(b"abcdef")
print(b[1:4], b[::-1])
print([1, 2, 3][slice(0, 2)])
print(slice(1, 2, 3).start, slice(1, 2, 3).stop, slice(1, 2, 3).step)
print(slice(None) == slice(None, None, None))
