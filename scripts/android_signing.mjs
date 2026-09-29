// Release tooling. Private material stays outside the checkout and is never printed.
import { mkdirSync, existsSync, readFileSync, writeFileSync, chmodSync } from "node:fs";
import { randomBytes } from "node:crypto";
import { join } from "node:path";
import { homedir } from "node:os";
import { spawnSync } from "node:child_process";

const directory = process.env.SUB2OPS_SIGNING_DIR || join(homedir(), ".config", "sub2ops", "android-signing");
const properties = join(directory, "release.properties");
const keystore = join(directory, "release.p12");
mkdirSync(directory, { recursive: true, mode: 0o700 });
if (!existsSync(properties)) {
  if (existsSync(keystore)) throw new Error("Existing signing key has no properties; refusing to replace it");
  const password = randomBytes(32).toString("hex");
  const result = spawnSync("keytool", ["-genkeypair", "-keystore", keystore, "-storetype", "PKCS12", "-storepass:env", "SUB2OPS_SIGNING_PASSWORD", "-keypass:env", "SUB2OPS_SIGNING_PASSWORD", "-alias", "sub2ops-release", "-keyalg", "RSA", "-keysize", "3072", "-validity", "10000", "-dname", "CN=Sub2Ops, O=lich13"], { env: { ...process.env, SUB2OPS_SIGNING_PASSWORD: password }, stdio: "pipe" });
  if (result.status !== 0) throw new Error("Release signing key creation failed");
  chmodSync(keystore, 0o600);
  writeFileSync(properties, `storeFile=${keystore}\nstorePassword=${password}\nkeyAlias=sub2ops-release\nkeyPassword=${password}\n`, { mode: 0o600, flag: "wx" });
}
if (process.argv.includes("--github")) {
  for (const [name, value] of [
    ["SUB2OPS_ANDROID_KEYSTORE", readFileSync(keystore).toString("base64")],
    ["SUB2OPS_ANDROID_SIGNING", readFileSync(properties, "utf8").replace(/^storeFile=.*$/m, "storeFile=/tmp/sub2ops-android-release.p12")],
  ]) {
    const result = spawnSync("gh", ["secret", "set", name, "--repo", "lich13/sub2api-ops-companion"], { input: value, stdio: ["pipe", "pipe", "pipe"] });
    if (result.status !== 0) throw new Error(`Could not store repository secret ${name}`);
  }
  console.log("Android release signing secrets configured");
}
console.log(`SUB2OPS_ANDROID_SIGNING_PROPERTIES=${properties}`);
