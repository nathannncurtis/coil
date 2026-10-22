from cryptography.fernet import Fernet

key = Fernet.generate_key()
token = Fernet(key).encrypt(b"coil compat")
assert Fernet(key).decrypt(token) == b"coil compat"
print("COMPAT_OK cryptography")
