from lxml import etree

root = etree.fromstring("<a><b id='1'>x</b><b id='2'>y</b></a>")
assert root.xpath("//b[@id='2']/text()") == ["y"]
print("COMPAT_OK lxml", etree.LXML_VERSION)
