"""Mapping of Python import names to PyPI package names.

Many packages have import names that differ from their PyPI distribution name.
This module maintains a mapping for common cases.
"""

# Import name -> PyPI package name
IMPORT_TO_PYPI: dict[str, str] = {
    "PIL": "Pillow",
    "cv2": "opencv-python",
    "bs4": "beautifulsoup4",
    "sklearn": "scikit-learn",
    "skimage": "scikit-image",
    "gi": "PyGObject",
    "wx": "wxPython",
    "yaml": "PyYAML",
    "attr": "attrs",
    "dotenv": "python-dotenv",
    "jose": "python-jose",
    "jwt": "PyJWT",
    "magic": "python-magic",
    "serial": "pyserial",
    "usb": "pyusb",
    "git": "GitPython",
    "dateutil": "python-dateutil",
    "docx": "python-docx",
    "pptx": "python-pptx",
    "lxml": "lxml",
    "Crypto": "pycryptodome",
    "OpenSSL": "pyOpenSSL",
    "psutil": "psutil",
    "win32com": "pywin32",
    "win32api": "pywin32",
    "win32gui": "pywin32",
    "pythoncom": "pywin32",
    "pywintypes": "pywin32",
    "googleapiclient": "google-api-python-client",
    "google": "google-cloud-core",
    "boto3": "boto3",
    "botocore": "botocore",
    "dns": "dnspython",
    "nacl": "PyNaCl",
    "socks": "PySocks",
    "pkg_resources": "setuptools",
    "setuptools": "setuptools",
    "toml": "toml",
    "tomli": "tomli",
    "chardet": "chardet",
    "charset_normalizer": "charset-normalizer",
    "urllib3": "urllib3",
    "certifi": "certifi",
    "idna": "idna",
}


def resolve_package_name(import_name: str) -> str:
    """Resolve a Python import name to its PyPI package name.

    If the import name is in the known mapping, returns the PyPI name.
    Otherwise, assumes the import name matches the package name.
    """
    return IMPORT_TO_PYPI.get(import_name, import_name)
