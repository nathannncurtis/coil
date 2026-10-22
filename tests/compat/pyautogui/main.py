import pyautogui

width, height = pyautogui.size()
x, y = pyautogui.position()
assert width > 0 and height > 0, (width, height)
assert 0 <= x <= width and 0 <= y <= height, (x, y)
print("COMPAT_OK pyautogui", width, height)
