// src/browser/host-vfs.mjs
var MODULE_ABI_VERSION = 1;
var KIND_MISSING = 0;
var KIND_FILE = 1;
var KIND_DIRECTORY = 2;
var OPEN_READ = 1 << 0;
var OPEN_WRITE = 1 << 1;
var OPEN_CREATE = 1 << 2;
var OPEN_EXCLUSIVE = 1 << 3;
var OPEN_TRUNCATE = 1 << 4;
var OPEN_APPEND = 1 << 5;
var ENOENT = -2;
var EIO = -5;
var EBADF = -9;
var EACCES = -13;
var EEXIST = -17;
var EROFS = -30;
var ESTALE = -116;
function createHostImports({ memory, backend, onError }) {
  const view = () => new Uint8Array(memory().buffer);
  const data = () => new DataView(memory().buffer);
  const decoder = new TextDecoder;
  const readPath = (pointer, length) => decoder.decode(view().subarray(pointer, pointer + length));
  const guard = (action) => (...args) => {
    try {
      return action(...args);
    } catch (error) {
      onError?.(error instanceof Error ? error.message : String(error));
      return errno(error);
    }
  };
  return {
    fylo_host: {
      open: guard((pathPointer, pathLength, flags, handlePointer) => {
        const handle = backend.open(readPath(pathPointer, pathLength), flags);
        data().setBigUint64(handlePointer, BigInt(handle), true);
        return 0;
      }),
      close: guard((handle) => {
        backend.close(Number(handle));
        return 0;
      }),
      read_at: guard((handle, offset, buffer, length, readPointer) => {
        const read = backend.readAt(Number(handle), Number(offset), view().subarray(buffer, buffer + length));
        data().setUint32(readPointer, read, true);
        return 0;
      }),
      write_at: guard((handle, offset, buffer, length, writtenPointer) => {
        const written = backend.writeAt(Number(handle), Number(offset), view().subarray(buffer, buffer + length));
        data().setUint32(writtenPointer, written, true);
        return 0;
      }),
      truncate: guard((handle, length) => {
        backend.truncate(Number(handle), Number(length));
        return 0;
      }),
      flush: guard((handle) => {
        backend.flush(Number(handle));
        return 0;
      }),
      stat: guard((pathPointer, pathLength, out) => {
        const entry = backend.stat(readPath(pathPointer, pathLength));
        const fields = data();
        fields.setUint32(out, entry.kind, true);
        fields.setBigUint64(out + 8, BigInt(entry.len ?? 0), true);
        fields.setBigUint64(out + 16, BigInt(entry.modifiedMs ?? 0), true);
        return 0;
      }),
      mkdir: guard((pathPointer, pathLength, recursive) => {
        backend.mkdir(readPath(pathPointer, pathLength), recursive !== 0);
        return 0;
      }),
      unlink: guard((pathPointer, pathLength) => {
        backend.unlink(readPath(pathPointer, pathLength));
        return 0;
      }),
      rmdir: guard((pathPointer, pathLength, recursive) => {
        backend.rmdir(readPath(pathPointer, pathLength), recursive !== 0);
        return 0;
      }),
      rename: guard((fromPointer, fromLength, toPointer, toLength) => {
        backend.rename(readPath(fromPointer, fromLength), readPath(toPointer, toLength));
        return 0;
      }),
      read_attrs: guard((pathPointer, pathLength, buffer, capacity, neededPointer) => {
        const manifest = backend.readAttrs(readPath(pathPointer, pathLength));
        data().setUint32(neededPointer, manifest.length, true);
        if (manifest.length <= capacity)
          view().set(manifest, buffer);
        return 0;
      }),
      write_attrs: guard((pathPointer, pathLength, manifest, length) => {
        backend.writeAttrs(readPath(pathPointer, pathLength), view().subarray(manifest, manifest + length));
        return 0;
      }),
      log: (pointer, length) => {
        backend.log?.(readPath(pointer, length));
      },
      now_unix_ms: guard((out) => {
        data().setBigUint64(out, BigInt(backend.nowUnixMs()), true);
        return 0;
      }),
      random: guard((buffer, length) => {
        backend.random(view().subarray(buffer, buffer + length));
        return 0;
      }),
      read_dir: guard((pathPointer, pathLength, buffer, capacity, neededPointer) => {
        const names = backend.readDir(readPath(pathPointer, pathLength));
        const encoded = encodeNames(names);
        data().setUint32(neededPointer, encoded.length, true);
        if (encoded.length <= capacity)
          view().set(encoded, buffer);
        return 0;
      })
    }
  };
}
function encodeNames(names) {
  const encoder = new TextEncoder;
  const parts = names.map((name) => encoder.encode(`${name}\x00`));
  const total = parts.reduce((sum, part) => sum + part.length, 0);
  const encoded = new Uint8Array(total);
  let offset = 0;
  for (const part of parts) {
    encoded.set(part, offset);
    offset += part.length;
  }
  return encoded;
}
function errno(error) {
  switch (error?.code) {
    case "ENOENT":
    case "NotFoundError":
      return ENOENT;
    case "EEXIST":
      return EEXIST;
    case "EBADF":
      return EBADF;
    case "EACCES":
      return EACCES;
    case "EROFS":
      return EROFS;
    case "ESTALE":
      return ESTALE;
    default:
      return EIO;
  }
}
function exec(instance, ndjson, trace = () => {}) {
  const { fylo_alloc, fylo_free, fylo_exec, fylo_abi_version, memory } = instance.exports;
  if (fylo_abi_version() !== MODULE_ABI_VERSION) {
    throw new Error(`unsupported FYLO module ABI: ${fylo_abi_version()}`);
  }
  trace("abi-ok");
  const request = new TextEncoder().encode(ndjson);
  const requestPointer = fylo_alloc(request.length);
  trace("allocated");
  new Uint8Array(memory.buffer).set(request, requestPointer);
  trace("request-written");
  const packed = fylo_exec(requestPointer, request.length);
  trace("exec-returned");
  fylo_free(requestPointer, request.length);
  if (packed === 0n)
    return "";
  const pointer = Number(packed >> 32n);
  const length = Number(packed & 0xffffffffn);
  const response = new Uint8Array(memory.buffer).slice(pointer, pointer + length);
  fylo_free(pointer, length);
  return new TextDecoder().decode(response);
}

// src/browser/fxdb-names.mjs
var WORKING_COPY_DIRECTORY = "fylo-fxdb";
function isWorkingCopyName(name) {
  return typeof name === "string" && name.length <= 200 && /^[A-Za-z0-9][A-Za-z0-9 ._()-]*\.fxdb$/.test(name) && !name.includes("..");
}

// src/browser/single-file-vfs.mjs
var DEFAULT_DATABASE_PATH = "/database.fxdb";
function createFileBackend(file, { path = DEFAULT_DATABASE_PATH, reader } = {}) {
  const sync = reader ?? createReaderSync();
  const modifiedMs = file.lastModified ?? 0;
  return createSingleFileBackend({
    path,
    writable: false,
    size: () => file.size,
    modifiedMs: () => modifiedMs,
    readAt(offset, into) {
      const end = Math.min(offset + into.length, file.size);
      if (end <= offset)
        return 0;
      let bytes;
      try {
        bytes = sync.readAsArrayBuffer(file.slice(offset, end));
      } catch (error) {
        throw stale(error);
      }
      into.set(new Uint8Array(bytes));
      return bytes.byteLength;
    }
  });
}
function createAccessHandleBackend(handle, { path = DEFAULT_DATABASE_PATH, writable = true } = {}) {
  let modifiedMs = Date.now();
  return createSingleFileBackend({
    path,
    writable,
    size: () => handle.getSize(),
    modifiedMs: () => modifiedMs,
    readAt: (offset, into) => handle.read(into, { at: offset }),
    writeAt(offset, from) {
      const written = handle.write(from, { at: offset });
      modifiedMs = Date.now();
      return written;
    },
    truncate(length) {
      handle.truncate(length);
      modifiedMs = Date.now();
    },
    flush: () => handle.flush()
  });
}
function createRangeBackend(url, { path = DEFAULT_DATABASE_PATH, ...options } = {}) {
  const Request = options.XMLHttpRequest ?? globalThis.XMLHttpRequest;
  let validator = "";
  let modifiedMs = 0;
  function fetchRange(first, last) {
    const request = new Request;
    request.open("GET", url, false);
    request.responseType = "arraybuffer";
    request.setRequestHeader("Range", `bytes=${first}-${last}`);
    if (validator)
      request.setRequestHeader("If-Range", validator);
    request.send();
    if (request.status === 416)
      return { total: null, bytes: new Uint8Array };
    if (request.status === 200) {
      throw validator ? withCode(new Error(`${url} changed on the server since it was opened`), "ESTALE") : withCode(new Error(`${url} does not support byte-range requests`), "EIO");
    }
    if (request.status !== 206) {
      throw withCode(new Error(`range request for ${url} failed with HTTP ${request.status}`), request.status === 404 ? "ENOENT" : "EIO");
    }
    const range = /^bytes (\d+)-(\d+)\/(\d+)$/.exec(request.getResponseHeader("Content-Range") ?? "");
    if (!range || Number(range[1]) !== first) {
      throw withCode(new Error(`${url} answered a range it was not asked for`), "EIO");
    }
    return {
      total: Number(range[3]),
      bytes: new Uint8Array(request.response),
      validator: strongEtag(request.getResponseHeader("ETag")) || request.getResponseHeader("Last-Modified") || "",
      modified: Date.parse(request.getResponseHeader("Last-Modified") ?? "")
    };
  }
  const probe = fetchRange(0, 0);
  if (probe.total === null)
    throw withCode(new Error(`${url} is empty`), "EIO");
  const size = probe.total;
  validator = probe.validator ?? "";
  modifiedMs = Number.isFinite(probe.modified) ? probe.modified : 0;
  return createSingleFileBackend({
    path,
    writable: false,
    size: () => size,
    modifiedMs: () => modifiedMs,
    readAt(offset, into) {
      const end = Math.min(offset + into.length, size);
      if (end <= offset)
        return 0;
      const { bytes } = fetchRange(offset, end - 1);
      if (bytes.length !== end - offset) {
        throw withCode(new Error(`${url} returned ${bytes.length} of ${end - offset} bytes`), "EIO");
      }
      into.set(bytes);
      return bytes.length;
    }
  });
}
function createSingleFileBackend(file) {
  const path = normalize(file.path);
  const parent = path.slice(0, path.lastIndexOf("/")) || "/";
  const name = path.slice(path.lastIndexOf("/") + 1);
  const handles = new Map;
  let nextHandle = 1;
  const handleOf = (handle) => {
    const open = handles.get(handle);
    if (!open)
      throw withCode(new Error(`handle ${handle} is not open`), "EBADF");
    return open;
  };
  const refuse = (what) => {
    throw withCode(new Error(`${what} is refused: this host holds the single file ${path}`), file.writable ? "EACCES" : "EROFS");
  };
  return {
    path,
    writable: file.writable,
    size: file.size,
    open(requested, flags) {
      if (normalize(requested) !== path) {
        if (flags & OPEN_CREATE)
          refuse(`creating ${requested}`);
        throw withCode(new Error(`${requested} does not exist`), "ENOENT");
      }
      const write = (flags & OPEN_WRITE) !== 0;
      if (write && !file.writable)
        refuse(`opening ${path} for writing`);
      const handle = nextHandle++;
      handles.set(handle, { write });
      return handle;
    },
    close(handle) {
      handleOf(handle);
      handles.delete(handle);
    },
    readAt(handle, offset, into) {
      handleOf(handle);
      return file.readAt(offset, into);
    },
    writeAt(handle, offset, from) {
      const { writeAt } = file;
      if (!handleOf(handle).write || !writeAt)
        return refuse(`writing ${path}`);
      return writeAt(offset, from);
    },
    truncate(handle, length) {
      const { truncate } = file;
      if (!handleOf(handle).write || !truncate)
        return refuse(`truncating ${path}`);
      truncate(length);
    },
    flush(handle) {
      handleOf(handle);
      file.flush?.();
    },
    stat(requested) {
      const at = normalize(requested);
      if (at === path) {
        return { kind: KIND_FILE, len: file.size(), modifiedMs: file.modifiedMs() };
      }
      if (at === parent)
        return { kind: KIND_DIRECTORY, len: 0, modifiedMs: 0 };
      return { kind: KIND_MISSING };
    },
    readDir(requested) {
      if (normalize(requested) === parent)
        return [name];
      throw withCode(new Error(`${requested} is not a directory`), "ENOENT");
    },
    mkdir: (requested) => refuse(`creating directory ${requested}`),
    unlink: (requested) => refuse(`removing ${requested}`),
    rmdir: (requested) => refuse(`removing directory ${requested}`),
    rename: (from) => refuse(`renaming ${from}`),
    readAttrs(requested) {
      if (normalize(requested) !== path) {
        throw withCode(new Error(`${requested} does not exist`), "ENOENT");
      }
      return new Uint8Array;
    },
    writeAttrs: (requested) => refuse(`writing attributes of ${requested}`),
    random: (into) => crypto.getRandomValues(into),
    nowUnixMs: () => Date.now(),
    log: (message) => console.error(message)
  };
}
function createReaderSync() {
  if (typeof FileReaderSync !== "function") {
    throw new Error("FileReaderSync is unavailable; open the database inside a Worker");
  }
  return new FileReaderSync;
}
function normalize(path) {
  if (typeof path !== "string" || !path.startsWith("/")) {
    throw withCode(new Error("single-file host paths must be absolute"), "EACCES");
  }
  const parts = [];
  for (const part of path.split("/")) {
    if (part === "" || part === ".")
      continue;
    if (part === "..") {
      throw withCode(new Error(`${path} leaves the database directory`), "EACCES");
    }
    parts.push(part);
  }
  return `/${parts.join("/")}`;
}
function strongEtag(etag) {
  return etag && !etag.startsWith("W/") ? etag : "";
}
function stale(error) {
  if (error?.name === "NotReadableError") {
    return withCode(new Error("the file changed on disk since it was opened; open it again"), "ESTALE");
  }
  return error;
}
function withCode(error, code) {
  return Object.assign(error, { code });
}

// src/browser/fylo-single-file-worker.mjs
var COPY_CHUNK_BYTES = 4 * 1024 * 1024;
var backend;
var accessHandle;
var releaseLock;
var instance;
var probe;
var lastHostError;
var queue = Promise.resolve();
self.onmessage = (event) => {
  const message = event.data ?? {};
  queue = queue.then(() => handle(message));
};
async function handle(message) {
  const { id } = message;
  try {
    const result = await dispatch(message);
    const transfer = result?.bytes instanceof Uint8Array ? [result.bytes.buffer] : [];
    self.postMessage({ id, result, hostError: lastHostError }, transfer);
  } catch (error) {
    self.postMessage({
      id,
      error: error instanceof Error ? error.message : String(error),
      code: error?.code ?? error?.name,
      hostError: lastHostError
    });
  }
}
async function dispatch(message) {
  switch (message.op) {
    case "open":
      return await open(message);
    case "exec":
      if (!instance)
        throw new Error("the engine was not loaded; pass moduleUrl to open");
      return { ndjson: exec(instance, message.ndjson) };
    case "host":
      return opened().call(message);
    case "snapshot":
      return snapshot();
    case "close":
      close();
      return { closed: true };
    default:
      throw new Error(`unknown op: ${message.op}`);
  }
}
async function open({ id, source, path = DEFAULT_DATABASE_PATH, moduleUrl }) {
  if (backend)
    throw new Error("this Worker already has a database open");
  lastHostError = undefined;
  try {
    if (source?.kind === "file") {
      backend = createFileBackend(source.file, { path });
    } else if (source?.kind === "url") {
      backend = createRangeBackend(new URL(source.url, self.location.href).href, { path });
    } else if (source?.kind === "opfs") {
      await openWorkingCopy(source, path, (progress) => self.postMessage({ id, progress }));
    } else {
      throw new Error(`unknown database source: ${source?.kind}`);
    }
    probe = createProbe(backend);
    if (moduleUrl)
      instance = await instantiate(moduleUrl, backend);
    return describe();
  } catch (error) {
    close();
    throw error;
  }
}
async function instantiate(moduleUrl, target) {
  let created;
  const imports = createHostImports({
    memory: () => created.exports.memory,
    backend: target,
    onError: (message) => {
      lastHostError = message;
    }
  });
  created = (await WebAssembly.instantiateStreaming(fetch(moduleUrl), imports)).instance;
  return created;
}
async function openWorkingCopy({ name, copyFrom }, path, progress) {
  if (!isWorkingCopyName(name))
    throw new Error(`invalid working copy name: ${name}`);
  await acquireLock(`fylo-fxdb:${name}`);
  const root = await navigator.storage.getDirectory();
  const directory = await root.getDirectoryHandle(WORKING_COPY_DIRECTORY, { create: true });
  const file = await directory.getFileHandle(name, { create: Boolean(copyFrom) });
  accessHandle = await file.createSyncAccessHandle();
  if (copyFrom)
    await streamInto(accessHandle, copyFrom, progress);
  backend = createAccessHandleBackend(accessHandle, { path });
}
async function streamInto(handle, blob, progress) {
  handle.truncate(0);
  const total = blob.size;
  let copied = 0;
  const reader = blob.stream().getReader();
  for (;; ) {
    const { done, value } = await reader.read();
    if (done)
      break;
    let written = 0;
    while (written < value.length) {
      written += handle.write(value.subarray(written), { at: copied + written });
    }
    copied += value.length;
    progress({ copied, total });
  }
  if (copied !== total)
    throw new Error(`copied ${copied} of ${total} bytes`);
  handle.flush();
}
function snapshot() {
  const file = opened();
  if (file.backend.writable)
    accessHandle?.flush();
  const size = file.backend.size();
  const parts = [];
  for (let offset = 0;offset < size; offset += COPY_CHUNK_BYTES) {
    const chunk = new Uint8Array(Math.min(COPY_CHUNK_BYTES, size - offset));
    let read = 0;
    while (read < chunk.length) {
      const count = file.backend.readAt(file.handle, offset + read, chunk.subarray(read));
      if (count === 0)
        throw new Error(`the file ended at ${offset + read} of ${size} bytes`);
      read += count;
    }
    parts.push(chunk);
  }
  return { blob: new Blob(parts, { type: "application/vnd.d31ma.fxdb" }), size };
}
function describe() {
  const { kind, len, modifiedMs } = opened().call({ call: "stat" });
  return {
    path: backend?.path,
    writable: backend?.writable ?? false,
    kind,
    size: len,
    modifiedMs,
    engine: Boolean(instance),
    crossOriginIsolated: globalThis.crossOriginIsolated === true
  };
}
function opened() {
  if (!probe)
    throw new Error("no database is open");
  return probe;
}
function close() {
  probe?.close();
  probe = undefined;
  instance = undefined;
  backend = undefined;
  try {
    accessHandle?.close();
  } finally {
    accessHandle = undefined;
    releaseLock?.();
    releaseLock = undefined;
  }
}
async function acquireLock(name) {
  if (!globalThis.navigator?.locks)
    return;
  await new Promise((granted, failed) => {
    navigator.locks.request(name, { mode: "exclusive", ifAvailable: true }, (lock) => {
      if (!lock) {
        failed(Object.assign(new Error("this working copy is open in another tab or window"), { code: "EBUSY" }));
        return;
      }
      granted(undefined);
      return new Promise((release) => {
        releaseLock = release;
      });
    }).catch(failed);
  });
}
function createProbe(target) {
  const memory = new WebAssembly.Memory({ initial: 1 });
  const table = createHostImports({
    memory: () => memory,
    backend: target,
    onError: (message) => {
      lastHostError = message;
    }
  }).fylo_host;
  const SCRATCH = 0;
  const PATH = 64;
  const DATA = 4096;
  const encoded = new TextEncoder().encode(target.path);
  const view = () => new DataView(memory.buffer);
  const bytes = () => new Uint8Array(memory.buffer);
  const room = (length) => {
    const needed = DATA + length - memory.buffer.byteLength;
    if (needed > 0)
      memory.grow(Math.ceil(needed / 65536));
  };
  const check = (code, what) => {
    if (code !== 0) {
      throw Object.assign(new Error(`${what} failed with errno ${-code}: ${lastHostError}`), {
        code: `E${-code}`
      });
    }
  };
  const openHandle = (flags) => {
    bytes().set(encoded, PATH);
    check(table.open(PATH, encoded.length, flags, SCRATCH), "open");
    return view().getBigUint64(SCRATCH, true);
  };
  const handle = openHandle(target.writable ? OPEN_READ | OPEN_WRITE : OPEN_READ);
  return {
    backend: target,
    handle: Number(handle),
    call(request) {
      switch (request.call) {
        case "stat": {
          bytes().set(encoded, PATH);
          check(table.stat(PATH, encoded.length, SCRATCH), "stat");
          return {
            kind: view().getUint32(SCRATCH, true),
            len: Number(view().getBigUint64(SCRATCH + 8, true)),
            modifiedMs: Number(view().getBigUint64(SCRATCH + 16, true))
          };
        }
        case "read": {
          room(request.length);
          check(table.read_at(handle, BigInt(request.offset), DATA, request.length, SCRATCH), "read_at");
          const read = view().getUint32(SCRATCH, true);
          return { read, bytes: bytes().slice(DATA, DATA + read) };
        }
        case "write": {
          const from = new Uint8Array(request.bytes);
          room(from.length);
          bytes().set(from, DATA);
          check(table.write_at(handle, BigInt(request.offset), DATA, from.length, SCRATCH), "write_at");
          return { written: view().getUint32(SCRATCH, true) };
        }
        case "truncate":
          check(table.truncate(handle, BigInt(request.length)), "truncate");
          return { length: request.length };
        case "flush":
          check(table.flush(handle), "flush");
          return { flushed: true };
        case "openWrite":
          table.close(openHandle(OPEN_READ | OPEN_WRITE));
          return { opened: true };
        default:
          throw new Error(`unknown host call: ${request.call}`);
      }
    },
    close() {
      table.close(handle);
    }
  };
}
