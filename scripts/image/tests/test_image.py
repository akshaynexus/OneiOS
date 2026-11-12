"""Tests of the image tools on generated stand-in files (no Apple files, no network).

    scripts/image/tests/run.sh          all of them
"""
import hashlib
import io
import json
import os
import plistlib
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest
import zipfile

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.abspath(os.path.join(HERE, "..", "..", ".."))
sys.path.insert(0, os.path.join(ROOT, "scripts", "image"))
sys.path.insert(0, os.path.join(ROOT, "scripts", "fetch"))
sys.path.insert(0, HERE)

import fixtures          # noqa: E402
import imagetool as it   # noqa: E402
import remotezip         # noqa: E402

REAL_PROFILE = os.path.join(ROOT, "profiles", "ios-27.0.1-iPhone18,2.toml")
HAVE_ZSTD = shutil.which("zstd") is not None


class Case(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="oneios-test-")
        self.addCleanup(shutil.rmtree, self.tmp, True)

    def path(self, *parts):
        return os.path.join(self.tmp, *parts)

    def write(self, rel, data):
        full = self.path(rel)
        os.makedirs(os.path.dirname(full), exist_ok=True)
        with open(full, "wb" if isinstance(data, bytes) else "w") as f:
            f.write(data)
        return full


class ProfileTest(Case):
    def test_real_profile(self):
        p = it.load_profile(REAL_PROFILE)
        self.assertEqual((p["os"]["build"], p["os"]["device"], p["os"]["version"]), ("24A446", "iPhone18,2", "27.0.1"))
        self.assertEqual(p["os"]["chip_id"], 0x8150)
        self.assertTrue(p["ipsw"]["url"].startswith("https://updates.cdn-apple.com/"))
        self.assertTrue(p["ipsw"]["url"].endswith("iPhone18,2_27.0.1_24A446_Restore.ipsw"))
        roles = {m["role"] for m in p["member"]}
        self.assertEqual(roles, set(it.ROLES))
        # every member that is not AEA is pinned by a cryptographic hash, the AEA ones by size and CRC-32
        for m in p["member"]:
            self.assertTrue(m.get("aea") or "sha384" in m or "sha256" in m, m["role"])
        self.assertEqual(it.input_name(it.member_of(p, "os")), "043-69487-662.dmg")
        self.assertEqual(p["image"]["stage"], "/data/local/tmp/oneios/image")

    def test_rejects(self):
        with open(REAL_PROFILE) as f:
            good = f.read()
        cases = {
            "schema": good.replace("schema = 1", "schema = 2"),
            "role": good.replace('role = "restore"', 'role = "bogus"'),
            "crc": good.replace('crc32 = "7ceca2a1"', 'crc32 = "7ceca2a"'),
            "aea": good.replace("aea = true\nbuildmanifest_digest = \"c4", "buildmanifest_digest = \"c4"),
            "format": good.replace("format = 1", "format = 9"),
            "name": good.replace('os = "os.img"', 'os = "../os.img"'),
            "twice": good.replace('app = "app.img"', 'app = "os.img"'),
            "uuid": good.replace("1B7DB2B7-5ECA-4FDA-98AF-55DF8632E355", "not-a-uuid"),
            "chunk": good.replace("chunk = 1048576", "chunk = 1000"),
        }
        for name, text in cases.items():
            self.assertNotEqual(text, good, name)
            with self.assertRaises(it.Error, msg=name):
                it.load_profile(self.write(name + ".toml", text))


class RemoteZipTest(Case):
    def make(self):
        self.stored, self.packed = os.urandom(70000), b"compressible " * 9000
        z = self.path("a.zip")
        with zipfile.ZipFile(z, "w") as f:
            f.writestr(zipfile.ZipInfo("big.dmg.aea"), self.stored, zipfile.ZIP_STORED)
            f.writestr(zipfile.ZipInfo("Firmware/x.trustcache"), self.packed, zipfile.ZIP_DEFLATED)
        return z

    def test_list_and_get(self):
        z = remotezip.RemoteZip(self.make())
        self.assertEqual([i.filename for i in z.infos()], ["big.dmg.aea", "Firmware/x.trustcache"])
        log = []
        z.get("big.dmg.aea", self.path("big"), jobs=3, chunk=4096, log=log.append)
        z.get("x.trustcache", self.path("x"), jobs=2, chunk=1000, log=log.append)     # found by its base name
        with open(self.path("big"), "rb") as f:
            self.assertEqual(f.read(), self.stored)
        with open(self.path("x"), "rb") as f:
            self.assertEqual(f.read(), self.packed)
        self.assertEqual(sorted(os.listdir(self.tmp)), ["a.zip", "big", "x"])           # no .part left
        z.get("big.dmg.aea", self.path("big"), log=log.append)
        self.assertIn("already complete", log[-1])
        self.assertEqual(z.head("big.dmg.aea", 16), self.stored[:16])
        with self.assertRaises(remotezip.Error):
            z.find("missing")

    def test_resume(self):
        z = remotezip.RemoteZip(self.make())
        info, chunk = z.find("big.dmg.aea"), 4096
        # an interrupted run: chunks 0 and 2 done, the rest zero
        part = bytearray(len(self.stored))
        for k in (0, 2):
            part[k * chunk:(k + 1) * chunk] = self.stored[k * chunk:(k + 1) * chunk]
        self.write("big.part", bytes(part))
        self.write("big.part.done", f"{info.CRC:08x} {len(self.stored)} {chunk}\n0\n2\n")
        log = []
        z.get("big.dmg.aea", self.path("big"), jobs=1, chunk=chunk, log=log.append)
        fetched = [line for line in log if "%" in line]
        self.assertEqual(len(fetched), -(-len(self.stored) // chunk) - 2)
        with open(self.path("big"), "rb") as f:
            self.assertEqual(f.read(), self.stored)

    def test_bad_chunk_is_caught(self):
        z = remotezip.RemoteZip(self.make())
        info, chunk = z.find("big.dmg.aea"), 4096
        part = bytearray(len(self.stored))                    # chunk 0 claimed done but holds zeros
        self.write("big.part", bytes(part))
        self.write("big.part.done", f"{info.CRC:08x} {len(self.stored)} {chunk}\n0\n")
        with self.assertRaises(remotezip.Error) as e:
            z.get("big.dmg.aea", self.path("big"), chunk=chunk, log=lambda s: None)
        self.assertIn("CRC-32", str(e.exception))
        self.assertFalse(os.path.exists(self.path("big")))
        self.assertFalse(os.path.exists(self.path("big.part.done")))


class FetchTest(Case):
    def test_fetch_checks_and_records(self):
        ipsw, profile, content = fixtures.fake_ipsw(self.path("src"))
        it.cmd_check_ipsw([profile])
        indir = self.path("inputs")
        it.cmd_fetch([profile, indir, "-j", "2"])
        with open(os.path.join(indir, "inputs.json")) as f:
            record = json.load(f)
        self.assertEqual(set(record["members"]), set(it.ROLES))
        app = record["members"]["app"]
        self.assertEqual(app["sha256"], hashlib.sha256(content["001-00003-001.dmg"]).hexdigest())
        self.assertTrue(app["buildmanifest_digest_is_file_sha384"])
        self.assertIn("sha384", app["pinned"])
        self.assertTrue(os.path.isfile(os.path.join(indir, "001-00001-001.dmg.aea")))
        self.assertFalse(os.path.exists(os.path.join(indir, "unrelated.bin")))
        # the AEA header of the fetched stand-in names a key service
        it.cmd_aea_info([os.path.join(indir, "001-00001-001.dmg.aea")])
        # after the decryption the plain image stands for the member: no second download
        plain = os.path.join(indir, "001-00001-001.dmg")
        fixtures.fake_apfs(plain)
        it.record_plain(indir, "os", plain)
        os.unlink(plain + ".aea")
        it.cmd_fetch([profile, indir, "--only", "os"])
        self.assertFalse(os.path.exists(plain + ".aea"))

    def test_wrong_pin_stops(self):
        ipsw, profile, _ = fixtures.fake_ipsw(self.path("src"), wrong_pin="trust-app")
        with self.assertRaises(it.Error) as e:
            it.cmd_fetch([profile, self.path("inputs")])
        self.assertIn("sha384", str(e.exception))
        self.assertTrue(os.path.exists(self.path("inputs", "001-00003-001.dmg.trustcache.bad")))
        self.assertFalse(os.path.exists(self.path("inputs", "001-00003-001.dmg.trustcache")))

    def test_profile_that_does_not_match_the_ipsw(self):
        ipsw, profile, _ = fixtures.fake_ipsw(self.path("src"))
        with open(profile) as f:
            text = f.read()
        for old, new, word in (('path = "001-00003-001.dmg"', 'path = "001-00009-001.dmg"', "not in the IPSW"),
                               ('build = "0T1"', 'build = "0T2"', "the profile wants")):
            with open(profile, "w") as f:
                f.write(text.replace(old, new))
            with self.assertRaises(it.Error) as e:
                it.cmd_check_ipsw([profile])
            self.assertIn(word, str(e.exception))

    def test_aea_and_im4p(self):
        profile, fields = it.aea_info(fixtures.fake_aea())
        self.assertEqual(profile, 1)
        self.assertEqual(fields["com.apple.wkms.url"], b"https://keys.example.invalid")
        with self.assertRaises(it.Error):
            it.aea_info(b"not an archive")
        self.assertEqual(it.im4p_payload(fixtures.fake_im4p(b"krnl", b"x" * 5000)), ("krnl", b"x" * 5000))
        with self.assertRaises(it.Error):
            it.im4p_payload(b"\x30\x03abc")


class ApfsAndHashesTest(Case):
    def test_apfs_info(self):
        image = self.path("os.img")
        want = fixtures.fake_apfs(image, "FakeOS", role=1)
        info = it.apfs_info(image)
        self.assertEqual(info["container_offset"], 0)
        self.assertEqual(info["block_size"], 4096)
        self.assertEqual(info["volumes"], [{"name": "FakeOS", "uuid": want, "role": "system",
                                            "case_sensitive": True, "sealed": True}])
        with self.assertRaises(it.Error) as e:
            it.apfs_info(self.write("junk.img", b"\0" * 8192 + b"koly" + b"\0" * 508))
        self.assertIn("UDIF", str(e.exception))

    def test_apfs_tree(self):
        entries = [("private", 0o40755, 0, 0, []), ("private/var", 0o40755, 0, 0, []),
                   ("private/var/mobile", 0o40750, 501, 501, []),
                   ("private/var/mobile/a.plist", 0o100600, 501, 501, ["com.example.tag", "com.apple.decmpfs"]),
                   ("private/var/db", 0o40700, 0, 0, []), ("private/var/db/l", 0o120755, 0, 80, [])]
        for hashed in (True, False):
            image = self.path(f"tree-{hashed}.img")
            fixtures.fake_apfs_tree(image, entries, hashed)
            tree = it.ApfsTree(image)
            got = tree.walk("private/var")
            tree.close()
            self.assertEqual(sorted(got), [".", "db", "db/l", "mobile", "mobile/a.plist"])
            self.assertEqual(got["mobile/a.plist"], {"u": 501, "g": 501, "m": 0o100600, "x": ["com.apple.decmpfs", "com.example.tag"]})
            self.assertEqual((got["db/l"]["g"], got["mobile"]["m"]), (80, 0o40750))
        # what a mount with owners ignored showed: every entry the user's own; the records put the owners back
        meta = {".": {"t": "d", "m": 0o755, "u": 99, "g": 99}, "mobile": {"t": "d", "m": 0o750, "u": 99, "g": 99},
                "mobile/a.plist": {"t": "f", "m": 0o600, "u": 99, "g": 99, "x": {"com.example.tag": "dg=="}},
                "db": {"t": "d", "m": 0o700, "u": 99, "g": 99}, "db/l": {"t": "l", "m": 0o755, "u": 99, "g": 99}}
        it._owners_from_image(meta, [], image, "private/var")
        self.assertEqual([(meta[k]["u"], meta[k]["g"]) for k in ("mobile", "mobile/a.plist", "db/l")], [(501, 501)] * 2 + [(0, 80)])
        for broken, why in ((lambda m: m["db"].update(m=0o755), "mode"), (lambda m: m["mobile/a.plist"].pop("x"), "xattrs"),
                            (lambda m: m.pop("db/l"), "not copied"), (lambda m: m.update(extra={"t": "f", "m": 0o644}), "not in")):
            m = json.loads(json.dumps(meta))
            broken(m)
            with self.assertRaises(it.Error) as e:
                it._owners_from_image(m, [], image, "private/var")
            self.assertIn(why, str(e.exception))
        with self.assertRaises(it.Error):
            it.ApfsTree(image).walk("private/nothing")

    def test_hash_list(self):
        data = os.urandom(4096 * 5 + 123)
        image, hashes = self.write("a.img", data), self.path("a.img.hashes")
        size, digest = it.write_hashes(image, hashes, 4096)
        self.assertEqual((size, digest), (len(data), hashlib.sha256(data).hexdigest()))
        chunk, size, digests = it.read_hashes(hashes)
        self.assertEqual((chunk, size, len(digests)), (4096, len(data), 32 * 6))
        self.assertEqual(digests[-32:], hashlib.sha256(data[4096 * 5:]).digest())
        it.check_hashes(image, hashes)
        with open(image, "r+b") as f:
            f.seek(4096 * 3 + 7)
            f.write(b"\xff" if data[4096 * 3 + 7] != 0xFF else b"\0")
        with self.assertRaises(it.Error) as e:
            it.check_hashes(image, hashes)
        self.assertIn("chunk 3", str(e.exception))
        with self.assertRaises(it.Error):
            it.read_hashes(self.write("bad.hashes", b"OIHL1\0\0\0" + b"\0" * 10))


class SeedTest(Case):
    def build(self, owners=""):
        src = fixtures.fake_os_root(self.path("os"))
        var = os.path.join(src, "private", "var")
        count, skipped = it.tree_copy(var, self.path("data"), self.path("meta.json"))
        self.assertEqual((count, skipped), (9, []))
        # stand in for the image's real owners, which a fixture made by this user cannot have
        with open(self.path("meta.json")) as f:
            meta = json.load(f)
        for rel, e in meta["entries"].items():
            mobile = rel.startswith("mobile")
            e["u"], e["g"] = (501, 501) if mobile else (0, 0)
        meta["entries"]["mobile/Library/Preferences/com.example.kept.plist"]["x"] = {"com.example.tag": "AAEC"}
        with open(self.path("meta.json"), "w") as f:
            json.dump(meta, f)
        # what a hook does: rewrite one file, add a file and a folder, remove one
        prefs = self.path("data", "mobile", "Library", "Preferences")
        with open(os.path.join(prefs, "com.example.setup.plist"), "wb") as f:
            f.write(plistlib.dumps({"SetupDone": True}))
        with open(os.path.join(prefs, "com.example.new.plist"), "wb") as f:
            f.write(plistlib.dumps({"New": 1}))
        os.chmod(os.path.join(prefs, "com.example.new.plist"), 0o600)
        os.makedirs(self.path("data", "oneios", "tools"))
        with open(self.path("data", "oneios", "tools", "probe"), "w") as f:
            f.write("#!/bin/sh\n")
        os.chmod(self.path("data", "oneios", "tools", "probe"), 0o755)
        os.unlink(self.path("data", "root", ".profile"))
        self.write("owners.tsv", owners)
        out = self.path("data.seed")
        stats = it.seed_pack(self.path("data"), self.path("meta.json"), out, 1700000000, self.path("owners.tsv"))
        with tarfile.open(out) as tar:
            return stats, {m.name: m for m in tar.getmembers()}, out

    def test_owners_modes_times(self):
        stats, m, out = self.build()
        self.assertEqual(stats, (12, 4, 1))        # entries, added, rewritten
        kept = m["mobile/Library/Preferences/com.example.kept.plist"]
        self.assertEqual((kept.uid, kept.gid, kept.mtime), (501, 501, 1600000000))
        self.assertEqual(kept.pax_headers.get("LIBARCHIVE.xattr.com.example.tag"), "AAEC")
        changed = m["mobile/Library/Preferences/com.example.setup.plist"]
        self.assertEqual((changed.uid, changed.mtime), (501, 1700000000))
        new = m["mobile/Library/Preferences/com.example.new.plist"]
        self.assertEqual((new.uid, new.gid, new.mode, new.mtime), (501, 501, 0o600, 1700000000))   # owner of its folder
        tool = m["oneios/tools/probe"]
        self.assertEqual((tool.uid, tool.gid, tool.mode), (0, 0, 0o755))
        self.assertTrue(m["link"].issym() and m["link"].linkname == "mobile/Library")
        self.assertTrue(m["empty"].isdir())
        self.assertNotIn("root/.profile", m)
        self.assertEqual({(x.uname, x.gname) for x in m.values()}, {("", "")})
        with tarfile.open(out) as tar:
            data = plistlib.load(tar.extractfile("mobile/Library/Preferences/com.example.setup.plist"))
        self.assertEqual(data, {"SetupDone": True})

    def test_owner_rules(self):
        rules = "oneios\t501\t501\t-\tR\noneios/tools/probe\t0\t0\t4755\n/empty/\t33\t33\t700\n"
        _, m, _ = self.build(rules)
        self.assertEqual((m["oneios"].uid, m["oneios/tools"].uid), (501, 501))
        self.assertEqual((m["oneios/tools/probe"].uid, m["oneios/tools/probe"].mode), (0, 0o4755))
        self.assertEqual((m["empty"].uid, m["empty"].gid, m["empty"].mode), (33, 33, 0o700))
        with self.assertRaises(it.Error):
            it.read_owners(self.write("bad.tsv", "a b c\n"))
        with self.assertRaises(it.Error):
            it.read_owners(self.write("bad2.tsv", "../x\t0\t0\t-\n"))

    def test_same_bytes_every_time(self):
        _, _, out = self.build()
        first = it.sha256_file(out)
        shutil.rmtree(self.path("os"))
        shutil.rmtree(self.path("data"))
        _, _, out = self.build()
        self.assertEqual(it.sha256_file(out), first)


class SetTest(Case):
    def make_set(self, kernel=True, overlay=True):
        _, profile, _ = fixtures.fake_ipsw(self.path("src"))
        fixtures.fake_inputs(self.path("in"))
        p = it.load_profile(profile)
        s = self.path("set")
        shutil.rmtree(s, True)
        for m in p["member"]:
            out = p["image"]["files"].get(m["role"])
            if out:
                os.makedirs(os.path.dirname(os.path.join(s, out)), exist_ok=True)
                shutil.copyfile(self.path("in", it.input_name(m)), os.path.join(s, out))
        with tarfile.open(os.path.join(s, "data.seed"), "w") as tar:
            tar.addfile(tarfile.TarInfo("mobile"))
        if kernel:
            with open(os.path.join(s, "kernel.kc"), "wb") as f:
                f.write(b"\xcf\xfa\xed\xfe" + b"stand-in kernel" * 1000)
        if overlay:
            os.makedirs(os.path.join(s, "overlay", "trust"))
            with open(os.path.join(s, "overlay", "trust", "owner.bin"), "wb") as f:
                f.write(b"owner bundle")
        return p, s

    def test_manifest(self):
        p, s = self.make_set()
        m = it.make_manifest(p, s, seed_info={"format": "tar", "root": "/private/var", "entries": 1},
                             tweaks={"hook": "userland/image-hook"})
        # what the OneOS fork's verifier reads (oneos/interface.md, section 2)
        self.assertEqual((m["format"], m["name"], m["os"], m["version"], m["build"], m["device"]),
                         (1, "oneios-image", "iOS", "0.0.1", "0T1", "iTest1,1"))
        files = {f["path"]: f for f in m["files"]}
        self.assertNotIn("manifest.json", files)
        self.assertEqual(sorted(files), sorted(it.set_files(s)))
        for rel, f in files.items():
            self.assertEqual(f["size"], os.path.getsize(os.path.join(s, rel)), rel)
            self.assertEqual(f["sha256"], it.sha256_file(os.path.join(s, rel)), rel)
        os_img = files["os.img"]
        self.assertEqual((os_img["role"], os_img["format"], os_img["read_only"], os_img["hashes"]),
                         ("os", "apfs-raw", True, "os.img.hashes"))
        self.assertEqual(os_img["apfs"]["volumes"][0]["name"], "FakeOS")
        self.assertEqual(os_img["source"], {"component": "OS", "member": "001-00001-001.dmg.aea", "decrypted": True})
        self.assertEqual(files["os.img.hashes"]["role"], "hashes")
        self.assertEqual(files["trust/static.trustcache"]["format"], "img4-payload")
        self.assertEqual(files["data.seed"]["format"], "tar")
        self.assertEqual(files["kernel.kc"]["hashes"], "kernel.kc.hashes")
        self.assertEqual(files["overlay/trust/owner.bin"]["role"], "overlay")
        self.assertEqual(m["kernel"], "kernel.kc")
        self.assertEqual(m["data"]["volume_uuid"], "11111111-2222-3333-4444-555555555555")
        self.assertEqual(m["profile"]["sha256"], p["_sha256"])
        self.assertEqual(it.verify_dir(s)["id"], m["id"])
        # the same inputs give the same manifest, byte for byte
        with open(os.path.join(s, "manifest.json"), "rb") as f:
            first = f.read()
        it.make_manifest(p, s, seed_info={"format": "tar", "root": "/private/var", "entries": 1},
                         tweaks={"hook": "userland/image-hook"})
        with open(os.path.join(s, "manifest.json"), "rb") as f:
            self.assertEqual(f.read(), first)

    def test_manifest_refuses(self):
        p, s = self.make_set(kernel=False, overlay=False)
        self.assertIsNone(it.make_manifest(p, s)["kernel"])
        os.unlink(os.path.join(s, "trust", "app.root_hash"))
        with self.assertRaises(it.Error) as e:
            it.make_manifest(p, s)
        self.assertIn("trust/app.root_hash", str(e.exception))
        p, s = self.make_set()
        with open(os.path.join(s, "stray.bin"), "wb") as f:
            f.write(b"x")
        with self.assertRaises(it.Error):
            it.make_manifest(p, s)
        os.unlink(os.path.join(s, "stray.bin"))
        with open(os.path.join(s, "app.img"), "wb") as f:
            f.write(b"\0" * 8192)
        with self.assertRaises(it.Error):
            it.make_manifest(p, s)
        # an image that is not the one the fetch recorded
        p, s = self.make_set()
        with self.assertRaises(it.Error) as e:
            it.make_manifest(p, s, inputs={"members": {"os": {"plain_sha256": "0" * 64}}})
        self.assertIn("differs from the input", str(e.exception))

    def test_verify_catches(self):
        p, s = self.make_set()
        it.make_manifest(p, s)
        it.verify_dir(s)

        def broken(change, word, quick=False):
            copy = self.path("copy")
            shutil.rmtree(copy, True)
            shutil.copytree(s, copy)
            change(copy)
            with self.assertRaises(it.Error) as e:
                it.verify_dir(copy, quick)
            self.assertIn(word, str(e.exception))

        def flip(rel, at):
            def change(d):
                with open(os.path.join(d, rel), "r+b") as f:
                    f.seek(at)
                    byte = f.read(1)
                    f.seek(at)
                    f.write(bytes([byte[0] ^ 1]))
            return change

        def edit(fn):
            def change(d):
                path = os.path.join(d, "manifest.json")
                with open(path) as f:
                    m = json.load(f)
                fn(m)
                with open(path, "w") as f:
                    json.dump(m, f)
            return change

        broken(flip("os.img", 20000), "os.img: SHA-256")
        broken(flip("trust/static.trustcache", 3), "SHA-256")
        broken(flip("os.img.hashes", 40), "SHA-256")
        broken(lambda d: os.unlink(os.path.join(d, "systemos.img")), "missing systemos.img")
        broken(lambda d: open(os.path.join(d, "extra"), "w").close(), "not in the manifest: extra")
        broken(lambda d: open(os.path.join(d, "app.img"), "ab").write(b"x"), "bytes", quick=True)
        broken(edit(lambda m: m.update(format=2)), "newer")
        broken(edit(lambda m: m.update(name="other")), "not a OneiOS image manifest")
        broken(edit(lambda m: m["files"][0].update(path="../escape")), "not a set path")
        broken(edit(lambda m: m["files"].append(dict(m["files"][0]))), "twice")
        broken(edit(lambda m: m["files"][0].update(sha256="xyz")), "without path, size or sha256")
        broken(edit(lambda m: m.pop("build")), "build is missing")
        # a hash list that passes its own SHA-256 but belongs to other content
        def swap(d):
            data = os.urandom(os.path.getsize(os.path.join(d, "os.img")))
            with open(os.path.join(d, "x"), "wb") as f:
                f.write(data)
            it.write_hashes(os.path.join(d, "x"), os.path.join(d, "os.img.hashes"), 4096)
            os.unlink(os.path.join(d, "x"))
            edit(lambda m: [f.update(sha256=it.sha256_file(os.path.join(d, "os.img.hashes")))
                            for f in m["files"] if f["path"] == "os.img.hashes"])(d)
        broken(swap, "differs from its hash list")
        # unknown keys are ignored: a later writer may add them without a new format number
        edit(lambda m: m.update(future_key={"a": 1}))(s)
        it.verify_dir(s)

    @unittest.skipUnless(HAVE_ZSTD, "zstd is not installed")
    def test_archive(self):
        p, s = self.make_set()
        it.make_manifest(p, s)
        out = self.path("set.tar.zst")
        digest = it.pack(s, out)
        with open(out + ".sha256") as f:
            self.assertEqual(f.read(), f"{digest}  set.tar.zst\n")
        self.assertEqual(it.verify_archive(out)["build"], "0T1")
        raw = subprocess.run(["zstd", "-d", "-c", out], check=True, capture_output=True).stdout
        with tarfile.open(fileobj=io.BytesIO(raw)) as tar:
            members = tar.getmembers()
        self.assertEqual(members[0].name, "manifest.json")
        self.assertTrue(all(m.isreg() or m.isdir() for m in members))
        self.assertEqual(sorted(m.name for m in members if m.isreg()), sorted(["manifest.json"] + it.set_files(s)))
        self.assertEqual({(m.uid, m.gid, m.mtime) for m in members}, {(0, 0, 0)})
        self.assertEqual(it.pack(s, self.path("again.tar.zst")), digest)        # same bytes

        def repack(change):
            buf = io.BytesIO()
            with tarfile.open(fileobj=io.BytesIO(raw)) as src, tarfile.open(fileobj=buf, mode="w") as dst:
                change(src, dst)
            bad = self.path("bad.tar.zst")
            subprocess.run(["zstd", "-q", "-f", "-o", bad], input=buf.getvalue(), check=True)
            return bad

        def tampered(src, dst):
            for m in src:
                data = src.extractfile(m).read() if m.isreg() else None
                if m.name == "trust/app.trustcache":
                    data = b"X" + data[1:]
                dst.addfile(m, io.BytesIO(data) if data is not None else None)

        def link_added(src, dst):
            for m in src:
                dst.addfile(m, src.extractfile(m) if m.isreg() else None)
            link = tarfile.TarInfo("escape")
            link.type, link.linkname = tarfile.SYMTYPE, "/data"
            dst.addfile(link)

        def manifest_late(src, dst):
            members = list(src)
            for m in members[1:] + members[:1]:
                dst.addfile(m, src.extractfile(m) if m.isreg() else None)

        def one_missing(src, dst):
            for m in src:
                if m.name != "app.img":
                    dst.addfile(m, src.extractfile(m) if m.isreg() else None)

        for change, word in ((tampered, "differs from the manifest"), (link_added, "not a regular file"),
                             (manifest_late, "not manifest.json"), (one_missing, "missing app.img")):
            with self.assertRaises(it.Error) as e:
                it.verify_archive(repack(change))
            self.assertIn(word, str(e.exception))
        with open(out + ".sha256", "w") as f:
            f.write("0" * 64 + "  set.tar.zst\n")
        with self.assertRaises(it.Error):
            it.verify_archive(out)


class BuildScriptTest(Case):
    """scripts/image/build.sh from stand-in inputs to a verified archive."""

    def run_build(self, *args, env=None):
        e = dict(os.environ, INPUTS=self.path("in"), OUT=self.path("out"), **(env or {}))
        return subprocess.run([os.path.join(ROOT, "scripts", "image", "build.sh"), *args],
                              capture_output=True, text=True, env=e)

    def prepare(self):
        _, profile, _ = fixtures.fake_ipsw(self.path("src"))
        fixtures.fake_inputs(self.path("in"))
        fixtures.fake_os_root(self.path("osroot"))
        self.write("kernel.kc", b"\xcf\xfa\xed\xfe" + b"stand-in kernel" * 500)
        hook = self.write("hook.sh", """#!/bin/sh
set -eu
[ "$ONEIOS_HOOK_API" = 1 ] && [ "$ONEIOS_BUILD" = 0T1 ] && [ -d "$ONEIOS_OS_ROOT/private/var" ]
p=$ONEIOS_DATA/mobile/Library/Preferences
python3 - "$p/com.example.setup.plist" <<'PY'
import plistlib, sys
d = plistlib.load(open(sys.argv[1], "rb")); d["SetupDone"] = True
plistlib.dump(d, open(sys.argv[1], "wb"))
PY
mkdir -p "$ONEIOS_DATA/oneios" "$ONEIOS_OVERLAY/trust"
echo tool > "$ONEIOS_DATA/oneios/tool"
printf 'oneios\\t501\\t501\\t-\\tR\\n' >> "$ONEIOS_OWNERS"
echo bundle > "$ONEIOS_OVERLAY/trust/owner.bin"
echo '{"tweaks": ["skip-setup"]}' > "$ONEIOS_INFO"
""")
        os.chmod(hook, 0o755)
        return profile, hook

    @unittest.skipUnless(HAVE_ZSTD, "zstd is not installed")
    def test_fake_set(self):
        profile, hook = self.prepare()
        r = self.run_build(profile, "--os-root", self.path("osroot"), "--systemos-root", self.path("osroot"),
                           "--app-root", self.path("osroot"), "--kernel", self.path("kernel.kc"), "--hook", hook)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        s = self.path("out", "0T1")
        m = it.verify_dir(s)
        files = {f["path"]: f for f in m["files"]}
        self.assertEqual(sorted(files), sorted([
            "app.img", "app.img.hashes", "data.seed", "kernel.kc", "kernel.kc.hashes", "os.img", "os.img.hashes",
            "overlay/trust/owner.bin", "systemos.img", "systemos.img.hashes", "trust/app.root_hash",
            "trust/app.trustcache", "trust/os.root_hash", "trust/static.trustcache", "trust/system.root_hash",
            "trust/system.trustcache"]))
        self.assertEqual(files["os.img"]["sha256"], it.sha256_file(self.path("in", "001-00001-001.dmg")))
        self.assertEqual(m["tweaks"]["info"], {"tweaks": ["skip-setup"]})
        self.assertEqual(m["tweaks"]["hook_sha256"], it.sha256_file(hook))
        self.assertEqual((files["data.seed"]["format"], files["data.seed"]["root"]), ("tar", "/private/var"))
        with tarfile.open(os.path.join(s, "data.seed")) as tar:
            seed = {x.name: x for x in tar.getmembers()}
            setup = plistlib.load(tar.extractfile("mobile/Library/Preferences/com.example.setup.plist"))
        self.assertEqual(setup, {"SetupDone": True})
        self.assertEqual((seed["oneios/tool"].uid, seed["oneios/tool"].mtime), (501, 1700000000))
        archive = self.path("out", "OneiOS_test.tar.zst")
        self.assertEqual(it.verify_archive(archive)["id"], "ios-0.0.1-0T1-iTest1-1")
        self.assertTrue(os.path.isfile(archive + ".sha256"))
        self.assertFalse(os.path.exists(self.path("out", ".work-0T1")))
        # verify.sh, the command the docs give
        v = subprocess.run([os.path.join(ROOT, "scripts", "image", "verify.sh"), archive], capture_output=True, text=True)
        self.assertEqual(v.returncode, 0, v.stderr)
        self.assertIn("ok:", v.stdout)

    def test_stops_with_one_line(self):
        profile, hook = self.prepare()
        common = ["--os-root", self.path("osroot"), "--no-pack"]
        cases = [
            (["--seed", "apfs", profile], "build: missing tools: "),
            ([*common, profile], "build: missing kernel collection"),
            ([*common, "--no-kernel", "--hook", self.path("nothing"), profile], "build: the hook"),
        ]
        if sys.platform != "linux":
            cases = cases[1:]
        for args, start in cases:
            r = self.run_build(*args)
            self.assertNotEqual(r.returncode, 0, args)
            self.assertEqual(len(r.stderr.strip().splitlines()), 1, r.stderr)
            self.assertTrue(r.stderr.startswith(start), r.stderr)
        os.unlink(self.path("in", "001-00002-001.dmg"))
        os.unlink(self.path("in", "001-00003-001.dmg.root_hash"))
        r = self.run_build(*common, "--no-kernel", profile)
        self.assertEqual(len(r.stderr.strip().splitlines()), 1, r.stderr)
        self.assertIn("missing inputs", r.stderr)
        self.assertIn("001-00002-001.dmg 001-00003-001.dmg.root_hash", r.stderr)
        self.assertFalse(os.path.exists(self.path("out", "0T1")))

    def test_failing_hook_leaves_no_set(self):
        profile, _ = self.prepare()
        hook = self.write("bad-hook.sh", "#!/bin/sh\nexit 3\n")
        os.chmod(hook, 0o755)
        r = self.run_build(profile, "--os-root", self.path("osroot"), "--systemos-root", self.path("osroot"),
                           "--app-root", self.path("osroot"), "--no-kernel", "--no-pack", "--hook", hook)
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("the userland hook failed", r.stderr)
        self.assertFalse(os.path.exists(self.path("out", "0T1")))

    def test_no_hook_no_kernel(self):
        profile, _ = self.prepare()
        r = self.run_build(profile, "--os-root", self.path("osroot"), "--no-kernel", "--no-hook", "--no-pack")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        m = it.verify_dir(self.path("out", "0T1"))
        self.assertIsNone(m["kernel"])
        self.assertIsNone(m["tweaks"])


if __name__ == "__main__":
    unittest.main()
