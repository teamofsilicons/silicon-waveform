import {
  createCipheriv,
  createDecipheriv,
  createHash,
  randomBytes,
} from "node:crypto";
import {
  closeSync,
  existsSync,
  fsyncSync,
  lstatSync,
  mkdirSync,
  openSync,
  readFileSync,
  writeFileSync,
} from "node:fs";
import { createRequire } from "node:module";
import { isAbsolute, join } from "node:path";

export const SESSION_TTL_MS = 900 * 86400_000;
export const COOKIE_TTL_MS = 400 * 86400_000;

/** Encrypted, synchronous storage for one gateway; the OS releases ownership on crash. */
export class SessionStore {
  constructor(directory, binding) {
    if (!isAbsolute(directory))
      throw new Error("WAVEFORM_SESSION_DIRECTORY must be absolute.");
    mkdirSync(directory, { recursive: true, mode: 0o700 });
    const check = (path, directory = false) => {
      const stat = lstatSync(path);
      if (
        !(directory ? stat.isDirectory() : stat.isFile()) ||
        stat.mode & 0o077 ||
        (process.getuid && stat.uid !== process.getuid())
      )
        throw new Error(
          "Waveform session storage must be private and owned by the gateway user.",
        );
    };
    check(directory, true);
    const keyPath = join(directory, "session.key"),
      path = join(directory, "sessions.sqlite");
    if (!existsSync(keyPath)) {
      if (existsSync(path))
        throw new Error(
          "Waveform session key is missing; restore it with its database.",
        );
      const fd = openSync(keyPath, "wx", 0o600);
      try {
        writeFileSync(fd, randomBytes(32));
        fsyncSync(fd);
      } finally {
        closeSync(fd);
      }
      const directoryFd = openSync(directory, "r");
      try {
        fsyncSync(directoryFd);
      } finally {
        closeSync(directoryFd);
      }
    }
    check(keyPath);
    this.key = readFileSync(keyPath);
    if (this.key.length !== 32)
      throw new Error("Invalid Waveform session encryption key.");
    if (!existsSync(path)) closeSync(openSync(path, "wx", 0o600));
    check(path);
    this.binding = createHash("sha256")
      .update(JSON.stringify(binding))
      .digest("hex");
    const { DatabaseSync } = createRequire(import.meta.url)("node:sqlite");
    this.database = new DatabaseSync(path);
    try {
      this.database
        .exec(`PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL; PRAGMA secure_delete=ON; PRAGMA locking_mode=EXCLUSIVE;
        BEGIN EXCLUSIVE; CREATE TABLE IF NOT EXISTS sessions (id TEXT PRIMARY KEY, payload BLOB NOT NULL) STRICT; COMMIT;`);
    } catch {
      this.database.close();
      throw new Error(
        "Waveform session storage is unavailable or already owned by another gateway.",
      );
    }
  }
  hash(id) {
    return createHash("sha256").update(id).digest("hex");
  }
  aad(id) {
    return Buffer.from(`waveform-session-v1:${this.binding}:${id}`);
  }
  validate(session) {
    if (
      !/^[a-f0-9]{64}$/.test(session.id) ||
      !Number.isSafeInteger(session.until) ||
      !["production", "test"].includes(session.active) ||
      !session[session.active] ||
      session.production?.key ||
      (session.test && !session.test.key)
    )
      throw new Error("Invalid Waveform session or testing boundary.");
  }
  load() {
    return this.database
      .prepare("SELECT id,payload FROM sessions")
      .all()
      .map((row) => {
        try {
          const value = Buffer.from(row.payload),
            decipher = createDecipheriv(
              "aes-256-gcm",
              this.key,
              value.subarray(0, 12),
            );
          decipher.setAAD(this.aad(row.id));
          decipher.setAuthTag(value.subarray(12, 28));
          const session = JSON.parse(
            Buffer.concat([
              decipher.update(value.subarray(28)),
              decipher.final(),
            ]).toString("utf8"),
          );
          this.validate(session);
          if (this.hash(session.id) !== row.id)
            throw new Error("Invalid record");
          return session;
        } catch {
          throw new Error(
            "Waveform sessions cannot be authenticated; check the key and service configuration.",
          );
        }
      });
  }
  save(session) {
    this.validate(session);
    if (
      !session.production?.refresh &&
      !session.test?.key &&
      !session.pending &&
      !session.loginAttempt
    )
      return this.delete(session.id);
    const id = this.hash(session.id),
      nonce = randomBytes(12),
      cipher = createCipheriv("aes-256-gcm", this.key, nonce);
    cipher.setAAD(this.aad(id));
    const plain = JSON.stringify(session, (key, value) =>
      ["refreshing", "changing"].includes(key) ? undefined : value,
    );
    const value = Buffer.concat([cipher.update(plain, "utf8"), cipher.final()]);
    this.database
      .prepare(
        "INSERT INTO sessions(id,payload) VALUES (?,?) ON CONFLICT(id) DO UPDATE SET payload=excluded.payload",
      )
      .run(id, Buffer.concat([nonce, cipher.getAuthTag(), value]));
  }
  delete(id) {
    this.database.prepare("DELETE FROM sessions WHERE id=?").run(this.hash(id));
  }
  close() {
    if (!this.closed) {
      this.database.close();
      this.key.fill(0);
      this.closed = true;
    }
  }
}
