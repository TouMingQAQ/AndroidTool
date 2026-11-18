import com.android.apksig.ApkSigner;
import com.android.apksig.ApkVerifier;
import com.android.apksig.apk.ApkUtils;
import com.android.apksig.util.DataSources;
import java.io.File;
import java.io.RandomAccessFile;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.security.KeyStore;
import java.security.PrivateKey;
import java.security.cert.X509Certificate;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;

/** Uses the official apksig library; passwords are read from short-lived files. */
public final class AndroidToolSigner {
    public static void main(String[] args) throws Exception {
        if (args.length == 2 && args[0].equals("verify")) {
            verify(new File(args[1]), null);
            return;
        }
        if (args.length != 6) {
            throw new IllegalArgumentException("Expected input, output, keystore, alias and two password files");
        }
        char[] storePassword = null;
        char[] keyPassword = null;
        try {
            storePassword = password(args[4]);
            keyPassword = password(args[5]);
            KeyStore store = KeyStore.getInstance(new File(args[2]), storePassword);
            PrivateKey key = (PrivateKey) store.getKey(args[3], keyPassword);
            if (key == null) {
                throw new IllegalArgumentException("Signing alias has no private key");
            }
            List<X509Certificate> chain = new ArrayList<>();
            for (java.security.cert.Certificate certificate : store.getCertificateChain(args[3])) {
                chain.add((X509Certificate) certificate);
            }
            File input = new File(args[0]);
            File output = new File(args[1]);
            int minSdk;
            try (RandomAccessFile apk = new RandomAccessFile(input, "r")) {
                minSdk = ApkUtils.getMinSdkVersionFromBinaryAndroidManifest(
                    ApkUtils.getAndroidManifest(DataSources.asDataSource(apk)));
            }
            ApkSigner.SignerConfig signer = new ApkSigner.SignerConfig.Builder(
                args[3], key, chain).build();
            new ApkSigner.Builder(List.of(signer))
                .setInputApk(input)
                .setOutputApk(output)
                .setMinSdkVersion(minSdk)
                .setV1SigningEnabled(minSdk < 24)
                .setV2SigningEnabled(true)
                .setV3SigningEnabled(true)
                .setV4SigningEnabled(false)
                .setVerityEnabled(false)
                .setAlignmentPreserved(true)
                .setOtherSignersSignaturesPreserved(false)
                .build().sign();
            verify(output, chain.get(0));
        } finally {
            if (storePassword != null) Arrays.fill(storePassword, '\0');
            if (keyPassword != null) Arrays.fill(keyPassword, '\0');
        }
    }

    private static char[] password(String file) throws Exception {
        byte[] bytes = Files.readAllBytes(Path.of(file));
        try {
            return new String(bytes, StandardCharsets.UTF_8).toCharArray();
        } finally {
            Arrays.fill(bytes, (byte) 0);
        }
    }

    private static void verify(File apk, X509Certificate expected) throws Exception {
        int minSdk;
        try (RandomAccessFile input = new RandomAccessFile(apk, "r")) {
            minSdk = ApkUtils.getMinSdkVersionFromBinaryAndroidManifest(
                ApkUtils.getAndroidManifest(DataSources.asDataSource(input)));
        }
        ApkVerifier.Result result = new ApkVerifier.Builder(apk)
            .setMinCheckedPlatformVersion(Math.min(24, minSdk)).setMaxCheckedPlatformVersion(36)
            .build().verify();
        if (!result.isVerified() || !result.isVerifiedUsingV2Scheme()
                || !result.isVerifiedUsingV3Scheme()) {
            throw new IllegalStateException("APK signature verification failed: " + result.getAllErrors());
        }
        if (expected != null && (result.getSignerCertificates().size() != 1
                || !expected.equals(result.getSignerCertificates().get(0)))) {
            throw new IllegalStateException("APK signer certificate changed");
        }
        // Android may prefer verity over a normal digest even when both are present.
        for (var signer : result.getV2SchemeSigners()) {
            for (var digest : signer.getContentDigests()) {
                rejectVerity(digest.getSignatureAlgorithmId());
            }
        }
        for (var signer : result.getV3SchemeSigners()) {
            for (var digest : signer.getContentDigests()) {
                rejectVerity(digest.getSignatureAlgorithmId());
            }
        }
        System.out.println("APK v2/v3 signatures verified (APK verity disabled)");
    }

    private static void rejectVerity(int algorithm) {
        if (algorithm == 0x421 || algorithm == 0x423 || algorithm == 0x425) {
            throw new IllegalStateException("APK still contains a verity signature digest");
        }
    }
}
