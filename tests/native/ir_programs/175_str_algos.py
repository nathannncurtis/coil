def is_palindrome(s):
    t = "".join(c.lower() for c in s if c.isalnum())
    return t == t[::-1]
print(is_palindrome("A man, a plan, a canal: Panama"))
print(is_palindrome("hello"))
def word_freq(text):
    out = {}
    for w in text.split():
        out[w] = out.get(w, 0) + 1
    return out
print(word_freq("a b a"))
def caesar(s, k):
    out = []
    for c in s:
        base = ord("a") if c.islower() else ord("A") if c.isupper() else None
        if base is None:
            out.append(c)
        else:
            out.append(chr((ord(c) - base + k) % 26 + base))
    return "".join(out)
print(caesar("Hello, World!", 3))
print(caesar(caesar("abcXYZ", 5), -5))
def initials(name):
    return ".".join(w[0].upper() for w in name.split()) + "."
print(initials("ada lovelace"))
def rreverse(s):
    if len(s) <= 1:
        return s
    return rreverse(s[1:]) + s[0]
print(rreverse("recursion"))
def count_vowels(s):
    return sum(1 for c in s.lower() if c in "aeiou")
print(count_vowels("The quick brown fox"))
