"""Fallback mapping of Python import names to PyPI distribution names.

The canonical source of import→distribution mapping is
``importlib.metadata.packages_distributions()``, which introspects the Python
environment Coil is running in. This module is consulted **only** when that
mapping has no entry for a given top-level module — typically the fresh-clone
case where the dependency hasn't been installed in Coil's env yet.

Keeping this list lean is deliberate: the more the hand-map covers, the more
likely it drifts out of sync with upstream package renames. The distributions
we carry entries for are ones where the "fresh clone before install" failure
mode is common enough to be worth guarding.
"""

# Import name -> PyPI distribution name
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
    # pywin32 ships many top-level modules; cover the common ones so that a
    # fresh clone without pywin32 installed still resolves them to a single
    # distribution instead of trying to pip-install nonexistent packages.
    "win32api": "pywin32",
    "win32com": "pywin32",
    "win32con": "pywin32",
    "win32event": "pywin32",
    "win32file": "pywin32",
    "win32gui": "pywin32",
    "win32job": "pywin32",
    "win32pipe": "pywin32",
    "win32process": "pywin32",
    "win32service": "pywin32",
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
    """Resolve a Python import name to its PyPI distribution name via the fallback map.

    If the import name is in the hand-maintained mapping, returns the PyPI
    name. Otherwise, assumes the distribution name matches the import name.

    Prefer ``importlib.metadata.packages_distributions()`` for accurate
    resolution — this function is a fallback used by the resolver only when
    that metadata lookup has no entry for the module.
    """
    return IMPORT_TO_PYPI.get(import_name, import_name)
