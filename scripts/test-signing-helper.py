"""Integration regression: python scripts/test-signing-helper.py path/to/large.apk

Requires JDK 17+. Uses a disposable test key and never changes the input APK.
Checks a small fixture, a real >2 GiB APK, wrong passwords, signer identity,
v2/v3 digest algorithms, and native library alignment. Does not install apps.
"""

import argparse
import os
from pathlib import Path
import struct
import subprocess
import tempfile
import zipfile


def field(data, offset=0):
    size = struct.unpack_from('<I', data, offset)[0]
    end = offset + 4 + size
    if end > len(data):
        raise ValueError('Truncated signing field')
    return data[offset + 4:end], end


def signature_info(apk):
    with apk.open('rb') as stream:
        stream.seek(-min(apk.stat().st_size, 65557), 2)
        tail = stream.read()
        eocd = tail.rfind(b'PK\x05\x06')
        assert eocd >= 0, 'Missing ZIP directory'
        directory = struct.unpack_from('<I', tail, eocd + 16)[0]
        stream.seek(directory - 24)
        size, magic = struct.unpack('<Q16s', stream.read(24))
        assert magic == b'APK Sig Block 42'
        stream.seek(directory - size)
        pairs = stream.read(size - 24)
    result = {}
    offset = 0
    while offset < len(pairs):
        length, scheme = struct.unpack_from('<QI', pairs, offset)
        value = pairs[offset + 12:offset + 8 + length]
        offset += 8 + length
        if scheme not in (0x7109871a, 0xf05368c0):
            continue
        signers, _ = field(value)
        signer, end = field(signers)
        assert end == len(signers), 'Expected exactly one signer'
        signed_data, _ = field(signer)
        digests, end = field(signed_data)
        certificates, _ = field(signed_data, end)
        certificate, _ = field(certificates)
        algorithms = []
        cursor = 0
        while cursor < len(digests):
            digest, cursor = field(digests, cursor)
            algorithms.append(struct.unpack_from('<I', digest)[0])
        result[scheme] = (algorithms, certificate)
    return result


def check(apk, certificate):
    info = signature_info(apk)
    assert set(info) == {0x7109871a, 0xf05368c0}, 'Missing v2/v3 signature'
    for algorithms, signer in info.values():
        assert algorithms and not ({0x421, 0x423, 0x425} & set(algorithms)), algorithms
        assert signer == certificate, 'Signing certificate changed'


def data_offset(apk, entry):
    with apk.open('rb') as stream:
        stream.seek(entry.header_offset + 26)
        name, extra = struct.unpack('<HH', stream.read(4))
        return entry.header_offset + 30 + name + extra


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('apk', type=Path)
    args = parser.parse_args()
    source = args.apk.resolve()
    assert source.stat().st_size > 0x7fffffff, 'Supply an APK larger than 2 GiB'
    root = Path(__file__).resolve().parents[1]
    classpath = os.pathsep.join(str(root / 'assets/signing' / name) for name in
                               ('androidtool-signing.jar', 'apksig-8.13.0.jar'))
    work = root / 'target'
    work.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='signing test 中文 ', dir=work) as directory:
        scratch = Path(directory)
        password = scratch / 'password.txt'
        password.write_text('androidtool-test-only', encoding='utf-8')
        store = scratch / 'test.p12'
        subprocess.run(['keytool', '-genkeypair', '-keystore', str(store),
                        '-storetype', 'PKCS12', '-alias', 'regression', '-storepass',
                        'androidtool-test-only', '-keypass', 'androidtool-test-only',
                        '-dname', 'CN=AndroidTool Signing Regression', '-keyalg', 'RSA',
                        '-keysize', '2048', '-validity', '30'], check=True)
        certificate = subprocess.check_output(['keytool', '-exportcert', '-keystore',
                                              str(store), '-storepass',
                                              'androidtool-test-only', '-alias', 'regression'])
        java = ['java', f'-Djava.io.tmpdir={scratch}', '-cp', classpath, 'AndroidToolSigner']

        def sign(input_apk, output_apk, password_path=password, success=True):
            result = subprocess.run(java + [str(input_apk), str(output_apk), str(store),
                                           'regression', str(password_path), str(password_path)],
                                    capture_output=True)
            assert (result.returncode == 0) == success, result.stderr.decode(errors='replace')

        small = scratch / 'small.apk'
        with zipfile.ZipFile(source) as original, zipfile.ZipFile(small, 'w') as fixture:
            fixture.writestr('AndroidManifest.xml', original.read('AndroidManifest.xml'))
            fixture.writestr('assets/test.txt', b'AndroidTool signing regression')
        signed_small = scratch / 'small-signed.apk'
        sign(small, signed_small)
        check(signed_small, certificate)
        bad_password = scratch / 'bad-password.txt'
        bad_password.write_text('incorrect', encoding='utf-8')
        failed = scratch / 'failed.apk'
        sign(small, failed, bad_password, success=False)
        assert not failed.exists(), 'Failed signing produced an output'
        signed_large = scratch / 'large-signed.apk'
        sign(source, signed_large)
        check(signed_large, certificate)
        with zipfile.ZipFile(source) as original, zipfile.ZipFile(signed_large) as signed:
            for entry in original.infolist():
                if entry.filename.upper().startswith('META-INF/'):
                    continue
                output_entry = signed.getinfo(entry.filename)
                assert (entry.CRC, entry.file_size, entry.compress_type) == (
                    output_entry.CRC, output_entry.file_size, output_entry.compress_type)
                if entry.filename.endswith('.so') and entry.compress_type == zipfile.ZIP_STORED:
                    before = data_offset(source, entry)
                    after = data_offset(signed_large, output_entry)
                    for alignment in (4, 4096, 16384):
                        if before % alignment == 0:
                            assert after % alignment == 0, entry.filename
        print('PASS: >2 GiB APK, small APK, no verity, v2/v3, same test certificate, '
              'entry metadata/native alignment, Unicode/space paths, wrong password')


if __name__ == '__main__':
    main()
