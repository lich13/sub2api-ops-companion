import unittest

from verify_native_privacy import verify_native_binary


class NativePrivacyTests(unittest.TestCase):
    def test_rejects_mac_and_linux_home_paths_without_echoing_fixture(self):
        fixtures = (
            ("macOS", b"/Users/fixture-secret/Library/Developer/Xcode/DerivedData/app"),
            ("Linux", b"/home/fixture-secret/workspace/build/output"),
        )
        for platform, data in fixtures:
            with self.subTest(platform=platform):
                with self.assertRaises(ValueError) as raised:
                    verify_native_binary(data)
                self.assertNotIn("fixture-secret", str(raised.exception))

    def test_rejects_specific_macos_temporary_paths_without_echoing_fixture(self):
        fixtures = (
            ("var-T", b"/var/folders/ab/xy123/T/fixture-secret/build/output"),
            ("private-C", b"/private/var/folders/cd/z987/C/fixture-secret/cache"),
        )
        for location, data in fixtures:
            with self.subTest(location=location):
                with self.assertRaises(ValueError) as raised:
                    verify_native_binary(data)
                self.assertNotIn("fixture-secret", str(raised.exception))

    def test_allows_generic_prefix_resource_name_and_remapped_build_path(self):
        fixtures = (
            ("generic-system-prefix", b"metadata includes /var/folders only"),
            ("resource-name", b"Resources/128x128@2x.png"),
            ("remapped-build-path", b"/build/sub2api-ops-companion/src/main.rs"),
        )
        for label, data in fixtures:
            with self.subTest(label=label):
                verify_native_binary(data)


if __name__ == "__main__":
    unittest.main()
