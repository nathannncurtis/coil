import os
import sys

os.environ["QT_QPA_PLATFORM"] = "offscreen"

from PyQt6.QtCore import QTimer
from PyQt6.QtWidgets import QApplication, QLabel

app = QApplication(sys.argv)
label = QLabel("compat")
label.show()
QTimer.singleShot(100, app.quit)
code = app.exec()
assert code == 0, code
print("COMPAT_OK pyqt6", app.platformName())
