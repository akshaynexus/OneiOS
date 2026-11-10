"""OneiOS root tweaks: edits an extracted iOS 27 root and its Data tree for the xnuhost guest.

Run by the image build:  python3 -I -m roottweak apply ...   (see docs/userland.md)
Standard library only. Nothing here contains or reads Apple code; it edits property lists and places files.
"""

SCHEMA = 1
