# ADR 0033: Primary layout on a data drive (`<drive>/<app>/<namespace>/<collection>/<TTID>.json`)

- Status: **Accepted, not yet built** (2026-11-21). The user made every
  decision below in five rounds of questions through the hub on 2026-11-21
  (quoted in "Context"). Slices L1-1 to L1-13 build it. It ships in **its own
  release, after the next FYLO release** (the S5 release of ADR 0030). Until
  that release, ADRs 0010, 0029 and 0032 stay in force as written.
- Release changed (2026-11-21): the user decided "there should be no later FYLO release"; this ADR now ships in **the next FYLO release**, together with ADR 0034 and [ADR 0035](0035-fylos-own-sdk-in-the-store-shape.md). Where this ADR says "its own release, after the next FYLO release", read "the next FYLO release".
- Date: **2026-11-21**
- Owners: **FYLO maintainers**
- Replaces: **ADR 0029** (no root on a network filesystem), when this ADR
  ships. The `EREMOTEROOT` refusal by filesystem type goes away (section 6).
- Amends: **ADR 0003** (one owner per root becomes one owner per namespace,
  section 4), **ADR 0006** (shard width 0 comes back while a collection is
  small, section 8), **ADR 0010** (the sidecar format is chosen by a probe,
  not by hand, section 6), **ADR 0016** (one `.vdbx` per namespace, section
  10; large objects are ADR 0034), **ADR 0030** (temp-tier keys and `auto`
  rule 1, section 9) and **ADR 0032** (the drive rule adds a mount-point test,
  section 5).
- Related: ADR 0009 (queue), ADR 0013 (write-ahead log), ADR 0015 (segments),
  ADR 0017 (schemas), ADR 0021 (identities), ADR 0024 (change feed), ADR 0028
  (invocation mode), ADR 0034 (large objects in `.vdbx`).
- Spec: FYLO-L1, `L1-spec.md` (outside the repository, in the FYLO logs
  folder), sections 8 to 15. Where the spec and this ADR differ, this ADR wins.

## Context

The user's request, 2026-11-21, quoted exactly:

> "for FYLO project i think we should have a the primary database in this
> format `<app_name>/<namespace>/<collection>/<TTID>.json` and it should
> always be at the root of an external disk/volume/drive"

Today a document is `<root>/.collections/<collection>/docs/<shard>/<TTID>.json`.
The root can be any folder on a local disk that is not the system disk (ADR
0032). Its lease files sit beside it, in its parent folder. FYLO has no app
level and no namespace level. In the code, the word "namespace" means
`.collections` versus `.buckets`.

The user's answers, quoted exactly. A later round replaces an earlier answer
to the same question.

| Question | The user's answer |
| -------- | ----------------- |
| Q1 What is a namespace? | "its basically the equivalent of a `schema` in Postgres" |
| Q2 What is "the root of a drive"? | "this mount point of any external data disk that is not the internal disk" |
| Q3 Where do non-document files go? | "hidden folder per collection and static files like images, videos -- files that are not `.json` should share the same folder as the `.json` files" |
| Q5 Where does the app name come from? | "Folder name = app (Rec.)" (replaced "New --app setting (Rec.)") |
| Q6 Shard folders | "Shard only when big" |
| Q7 Several apps on one drive | "Yes, several (Rec.)" |
| Q8 `.vdbx` | "<drive>/<app>/<namespace>.vdbx (Rec.)" |
| Q9 Existing databases | "Offline copy command (Rec.)" |
| Q2b Depth below the mount point | "for testing we can use namespaces feature for the slices" |
| N1 How a file links to a document | "Id stored in a field (Rec.)" |
| N5 File-system type | "FYLO should not care about what type of file system is being mounted on. it should just have a guard that says it should be mounted on an external drive" |
| N6 App mode (ADR 0032) | "Keep as the exception (Rec.)" |
| N7 Files in `.vdbx` | "Design it now" (ADR 0034) |
| N8 When a collection grows | "Automatic (Rec.)" |
| N9 Unit of ownership | "One lock per namespace (Rec.)" |
| N11 Memory file systems | "Keep refusing (Rec.)" |
| N12 Staying correct on any file system | "Test drive once (Rec.)" |
| N13 What turns the temp tier on | "Network file system (Rec.)" |
| N14 Test images where ATLAS cannot be reached (replaced, see "Test images" below) | "Local image in each, but also look into figuring out a way to mount ATLAS on Linux and Windows VMs on the mac" |
| N15 windows-vm test images (replaced, see "Test images" below) | "figure out how to mount ATLAS volume on windows VM" |
| N16 What "external drive" means | "Not the boot disk (Rec.)" |
| Q4 Tests and CI | "Real disk images" |
| Test images (replaces N4 "On the ATLAS drive", N14 and N15 for test images) | "actually i've changed my mind, for testing lets create a virtual disk image that can be used for all FYLO testing instead of ATLAS as ATLAS is used for production data"; where the file lives: "HANGAR SSD (Rec.)" |
| N17 One image for all testing, or one per system | "One image per system (Rec.)" |

Facts that decide the design:

- **Lease files cannot stay in the drive's top folder's parent.** If the
  drive's top folder were itself a root, its lease files would go to
  `/Volumes` (macOS, not writable), nowhere (Windows `D:\` has no parent) or
  `/mnt` (Linux, often read-only in containers). So the root is
  `<drive>/<app>`, and every lease file lives inside the drive.
- **What operating systems report** (spec section 12, measured on this Mac,
  inside the Mesail dev host, and from the Windows documentation): "external"
  is reported only for physical USB or Thunderbolt disks, and every disk
  image is called "external". Inside VMs, containers, cloud machines, CI
  runners and Lambda no disk is ever "external". Every OS reports **which
  disk it boots from**. This is why N16 is "not the boot disk".
- **Kernel file locks are not enough on every file system.** On an unfsd
  NFSv3 mount, two machines both held the root lock and both wrote (Mesail
  trial, 2026-11-12, ADR 0029). On ATLAS over NFSv4 the lock worked. FYLO
  cannot tell these cases apart from the client. So "do not care about the
  type" (N5) needs a lease that is safe without trusting the kernel lock.
- **Users pass one full root path per app today.** TACHYON:
  `fylo exec --loop --root <path>` from `fylo://<path>`. Mesail:
  `fylo:///mnt/data/<service>`. Keeping `--root <drive>/<app>` keeps both.
- **Mesail's parallel gate slices** each start their own `fylo` process. They
  become namespaces of one app (Q2b), so two processes must be able to own
  two namespaces of one app at the same time (N9).

## Decision

### 1. The layout

```text
<mount point of a data drive>/                 not the boot disk (section 5)
  <app>/                                       --root; the app name is this folder's name
    .fylo/                                     app-wide state
      format.json                              layout marker "fylo.layout.v2" and the recorded probe result
      namespaces.lock                          short lock, held only while a namespace is created or dropped
      trash/                                   dropped namespaces, deleted after the drop
    .collections  .buckets  .fylo-catalog      three empty plain FILES: old binaries refuse this folder
    .<ns>.fylo-root-owner.lock                 the namespace's kernel lock file (section 4)
    .<ns>.fylo-root-owner.lock.json            the namespace's lease record (heartbeat, fencing)
    .<ns>.fylo-root-turn.lock                  the namespace's fair-wait turn lock (invocation mode)
    <ns>/                                      a namespace, like a Postgres schema; default "public"
      .fylo/                                   namespace state: catalog, structural journal,
                                               schema registry, queue, version control
      .collections  .buckets  .fylo-catalog    the same three guard files
      <collection>/
        <TTID>.json                            a document
        <TTID>.<ext>                           a file (image, video, any bytes), beside the documents
        .<TTID>.<ext>.fylo-attrs               sidecar metadata, only when the probe chose sidecars
        .fylo/                                 this collection's derived files: index, write-ahead
                                               log, deleted items, locks, segments, stamps, change feed
        <S>/<TTID>.json, <S>/<TTID>.<ext>      the same, after the collection grew past 10,000 entries
    <ns>.vdbx                                  a namespace kept as one file instead of a folder
```

Rules:

- **The visible tree is primary data only.** Every derived or internal file
  of a collection is in `<collection>/.fylo/`. Names of apps, namespaces and
  collections cannot start with `.`, so `.fylo` never clashes with them, nor
  with a TTID.
- **Soft-deleted items** move to `<collection>/.fylo/deleted/` (same disk,
  so the move is an atomic rename). `restore` reads them from there.
- **Guard files.** An old FYLO binary looks for the folders `.collections`,
  `.buckets` and `.fylo-catalog`. In a new app folder and in every namespace
  folder they are empty plain files, so an old binary fails when it tries to
  use them as folders, instead of starting an empty database. ADR 0015
  already uses this trick (a plain file named `docs`).
- **`format.json`** names the layout (`fylo.layout.v2`), the FYLO version
  that created the app folder, and the probe result (section 6). A folder
  without it is not a FYLO app folder (section 5.4).
- **Backups.** The write-ahead log (ADR 0013) can hold the newest commits
  until a checkpoint. A copy of the visible tree is complete only after
  `fylo checkpoint` or a clean stop. `fylo pack` is the safe backup of a live
  namespace.

### 2. App names and name rules

- **The app name is the root's last folder name** (Q5): `--root
  /mnt/data/chat` means drive `/mnt/data` and app `chat`. There is no `--app`
  flag. TACHYON's `fylo://` URLs and Mesail's `/mnt/data/<service>` roots
  keep their form. The name is read after the path is made canonical
  (symbolic links resolved).
- **One rule for app, namespace and collection names:**
  - lowercase ASCII `a-z`, digits `0-9` and `-`;
  - 2 to 64 bytes (64 bytes is a new maximum for collection names);
  - the first and last character is a letter or a digit;
  - refused: Windows device names (`con`, `prn`, `aux`, `nul`, `com1` to
    `com9`, `lpt1` to `lpt9`) and FYLO's reserved words (today's collection
    list: `sql`, `as`, `then`, `db`, `engine`, `cache`, and the rest).
  - Lowercase only, because APFS and NTFS treat `Mail` and `mail` as the same
    folder by default.
- **Errors** (new, stable, not retryable, sent like `ESYSTEMDISK`: an error
  frame before anything is written):
  - `EAPPNAME`: "the root's folder name `<name>` is not a valid FYLO app
    name: <reason>. Use 2-64 lowercase letters, digits or '-', starting and
    ending with a letter or digit." The reason is one of: too short, too
    long, character not allowed (names the first bad character), bad first
    or last character, reserved name.
  - `ENAMESPACE`: the same message for a namespace name, and for a namespace
    that exists both as a folder and as a `.vdbx` (section 10).
  - A collection name that breaks the rule keeps today's error code.
- **With the opt-out** (section 5.3) a folder name that breaks the rule is
  admitted (temporary folders such as `.tmpAbC123` break it). The handshake
  then reports `storage.appName: null`.

### 3. Namespaces work like Postgres schemas

Q1: "its basically the equivalent of a `schema` in Postgres".

- **The app is the database; a namespace is a schema.** A collection name is
  unique inside its namespace: `a.users` and `b.users` are two collections.
- **Default namespace `public`**, as in Postgres.
- **Choosing a namespace:**
  - every machine request may carry an optional field `"namespace"`;
  - `fylo exec --loop --root <root> --namespace <ns>` sets the default for
    requests that name none (TACHYON passes it from the datasource URL, for
    example `fylo:///mnt/data/chat?namespace=slice-12`);
  - the other CLI commands take `--namespace <ns>`;
  - SQL accepts `namespace.collection` (`SELECT * FROM billing.invoices`). A
    name without a namespace means the request's namespace.
- **Reads across namespaces are allowed**: a query, `joinDocs` or a SQL join
  may read collections of several namespaces of one app, but only when the
  process can hold every namespace's lease (section 4). If another process
  owns one of them, the read fails with `EROOTLOCKED`. This is stricter than
  Postgres, which has a server; FYLO has none.
- **No transactions across namespaces.** FYLO has no transactions across
  collections today, even in one namespace. A namespace does not change that.
  If FYLO adds multi-collection transactions later, they must work across
  namespaces too.
- **New machine operations:** `createNamespace`, `dropNamespace` (with an
  explicit `cascade` flag; without it only an empty namespace can be
  dropped) and `listNamespaces`. **New CLI:** `fylo namespace
  create|drop|list --root <drive>/<app>`.
  - Create: under `<app>/.fylo/namespaces.lock`, make `<app>/<ns>/` with its
    `.fylo/` folder and guard files.
  - Drop: take the namespace's lease (so a drop fails with `EROOTLOCKED`
    while another process uses it), rename the folder into
    `<app>/.fylo/trash/<random>`, release the lease, then delete it. A crash
    leaves at most a folder in the trash; the next open of the app deletes
    it.
  - Callers that delete namespace folders with `rm -rf` (Mesail's gate
    scripts today) switch to `fylo namespace drop`, which refuses while the
    namespace is in use.
- **Permissions.** Folder roots have none. A protected `.vdbx` (ADR 0021) is
  one namespace (section 10), so its policy is per namespace, close to
  Postgres schema permissions.
- **Rename in the code.** Today's "namespace" (`.collections` versus
  `.buckets`) is renamed "kind" in slice L1-2. After L1-6 the kind goes away
  (section 7).

### 4. One lease per namespace, safe on any file system

N9: "One lock per namespace (Rec.)". N5 needs it to be safe where the kernel
lock may bind only one machine.

**Where.** The lease files of namespace `<ns>` sit beside its folder, inside
the app folder: `<app>/.<ns>.fylo-root-owner.lock`,
`<app>/.<ns>.fylo-root-owner.lock.json` and `<app>/.<ns>.fylo-root-turn.lock`.
FYLO writes nothing in the drive's top folder except the app folders it
creates.

**What a process owns.** A process owns a namespace when it holds both:

1. **the kernel lock** on `.<ns>.fylo-root-owner.lock`, as the root lock is
   taken today; and
2. **the current generation of the lease record** in
   `.<ns>.fylo-root-owner.lock.json`: its random owner id (new for every
   process start), a **generation** number (the fencing number) and a
   **heartbeat counter**.

**Heartbeat.** The owner adds one to the counter every **5 s** (a setting).
It writes the record to a temporary file in the app folder and renames it
over the record.

**Taking a namespace.**

1. Take the kernel lock. If another process on this machine holds it, wait
   or fail as today (`EROOTLOCKED`); local disks never get further than this
   while a local owner lives.
2. Read the record. If it is marked **released**, or there is none, claim the
   next generation (step 4) at once.
3. Otherwise the owner may live on another machine. Read the counter, wait
   the **takeover time (20 s**, a setting) **on this process's own clock**,
   and read again. Only if the counter did not change may it claim. No clock
   is compared across machines, so clock differences do not matter.
4. **Claim** generation `g+1` by creating
   `<app>/.<ns>.fylo-root-owner.claim.<g+1>` with exclusive create
   (`O_EXCL`; the probe checks it works, section 6). Only one process can
   create that name. The winner writes the record with generation `g+1` and
   removes older claim files. A loser stops with `EROOTLOCKED` or waits. A
   claim that does not become the record within the takeover time is itself
   stale, and the next process claims the generation after it.

**Fencing.** Before every commit (the commit point of ADR 0013 and ADR 0015)
the owner opens and reads the record and checks that it still names its
owner id and generation. A process that was paused and lost the namespace
stops with `EROOTLEASELOST` (the code exists today) before it writes. Reads
check ownership once per read frame, as today.

**Stopping.** A clean stop marks the record **released** (it does not delete
it, so the generation keeps growing and fencing stays sound) and releases the
kernel lock. The next owner starts at once. Only after a crash does the next
owner wait, up to the takeover time.

**One process, several namespaces.** An `exec --loop` takes a namespace's
lease when its first request for that namespace arrives, as it takes the
root's lease today, and keeps it until it stops. Two processes can own two
namespaces of one app at the same time. Invocation mode (ADR 0028) and the
fair-wait turn lock work per namespace.

**Costs** (estimates from the spec, to be measured in L1-3):

- heartbeat: one small write every 5 s per owned namespace; nothing
  measurable on a local disk, about 2-20 ms on NFS;
- fencing: one small read per commit; page cache on a local disk, about
  1-18 ms on NFS;
- after a crash, the next owner waits up to 20 s. In invocation mode a
  request in that window gets `EROOTLOCKED` after its deadline. A clean stop
  has no wait.

**What a namespace holds alone.** Its collections; each collection's
indexes, write-ahead log, deleted items, stamps and change feed (in
`<collection>/.fylo/`); its own catalog, structural journal, schema
registry, queue and version control (in `<ns>/.fylo/`); its own temp-tier
entries (section 9). The app folder holds only `format.json`, the guard
files, the lease files, `namespaces.lock` and the trash.

**Not changed:** `fsync` is trusted and cannot be probed. An NFS export with
`async` can lose the last seconds of writes when its server crashes. This is
documented, not checked.

### 5. The drive rule

#### 5.1 The rule

A root `<drive>/<app>` is admitted only when **all** of these hold. Each is
checked before anything is written, at the points where ADR 0032 checks
today (every `fylo exec` mode, the other commands and the library):

1. **The root's parent folder is a mount point** (Q2, Q2b: strict, no
   deeper folders). FYLO compares the parent with the mount point of its own
   file system: `statfs` on macOS, `/proc/self/mountinfo` on Linux,
   `GetVolumePathNameW` on Windows (a drive root such as `D:\`, or an NTFS
   folder mount point). A bind mount or a Docker volume counts: FYLO sees the
   mounts of its own container.
2. **The drive is not the boot disk** (N16: "Not the boot disk (Rec.)"). The
   boot disk is what each OS reports: macOS the boot volume's APFS container;
   Linux the device of `/` and its parent disks (partitions, LVM, ZFS pool);
   Windows the disk with `IsBoot` or `IsSystem`. This is ADR 0032's disk
   comparison, unchanged. A disk image counts as the disk that holds its
   file (ADR 0032 section 2.2). A network share has no disk; it is not the
   boot disk, so it passes.
3. **The file system is not a memory file system** (N11: "Keep refusing"):
   `tmpfs` and `ramfs` are refused, because their data is lost at every
   restart. This is the only file-system type FYLO still checks.

What passes: a USB or Thunderbolt disk, a second internal disk, a cloud data
volume, a VM's second virtual disk, a Docker volume, a disk image whose file
is not on the boot disk, and a network share (NFS, SMB, EFS) mounted at a
mount point.

What is refused: any folder on the boot disk (home folders, `/tmp`,
`$TMPDIR`, `C:\`), a second partition or volume of the boot disk, a
container's own root file system (overlay), a disk image whose file is on the
boot disk, a memory file system, and any folder that is not directly below a
mount point.

**Inside VMs and containers** FYLO judges from inside: it sees the VM's
virtual disks and the container's mounts, never the host's disks. A second
virtual disk passes even when its file is on the host's internal disk. The
host's operator is responsible for where that file lives (documented in the
README).

#### 5.2 The refusal

- `ENOTVOLUMEROOT` (new, stable, not retryable): "the root's parent `<parent>`
  is not a mount point; the nearest mount point above it is `<mount>`. Put
  the app folder directly in the drive's top folder, or use the opt-out."
- `ESYSTEMDISK` (ADR 0032) for the boot disk and for memory file systems,
  unchanged.
- The order: name (`EAPPNAME`), then mount point (`ENOTVOLUMEROOT`), then
  disk (`ESYSTEMDISK`).

#### 5.3 The opt-out and app mode stay

- **The ADR 0032 opt-out, in all three forms** (`--allow-system-disk`,
  `FYLO_ALLOW_SYSTEM_DISK=1`, `fylo.allowSystemDisk`), waives all three
  checks of section 5.1 and the app-name check, with the same single warning
  line on stderr. The handshake reports `placement: "systemDiskAllowed"`.
  TACHYON's tests, `ty dev` and `ty preview` use it with temporary folders.
- **App mode stays the one exception** (N6). With `--app-id <id>`
  (`FYLO_APP_ID`, `fylo.appId`), and always on iOS and Android, the app's own
  OS data folder plays the part of `<drive>/<app>`: namespaces live directly
  inside it. The app-name rule is not applied there (the id may contain dots
  and capitals); the handshake reports `storage.appName: null` and
  `storage.appId: "<id>"`. The browser (OPFS) has no drive and is not
  affected.

#### 5.4 Several apps on one drive

Q7: "Yes, several (Rec.)". Each app folder is its own set of namespaces and
leases. FYLO touches only app folders and the files inside them, never other
folders on the drive (Mesail keeps `/mnt/data/blobs` there). FYLO refuses to
adopt an existing, non-empty folder that has no `.fylo/format.json`, with the
new code `ENOTFYLOROOT`, instead of writing into it. An empty folder, or a
missing one, becomes a new app folder.

### 6. Any file system: a probe at creation

N5: "FYLO should not care about what type of file system is being mounted
on". N12: "Test drive once (Rec.)".

**ADR 0029 is replaced** when this ADR ships. There is no `EREMOTEROOT` by
file-system type. A root on NFS, SMB, EFS or another network share is
admitted when it passes section 5. Dropping a refusal does not break callers.
`EREMOTEROOT` stays in the error catalog as a retired code.

**The probe.** When FYLO creates an app folder it tests the file system once
(about ten small operations, under 50 ms) inside `<app>/.fylo/`:

| Need | Probe | When it fails |
| ---- | ----- | ------------- |
| Metadata (keys, custom `meta`, checksums) | set and read an extended attribute (Windows: an NTFS alternate data stream) | use **sidecar files** instead |
| Atomic replace | rename a file over another | refuse to create the app folder (`EFSPROBE`) |
| Exclusive create | `O_EXCL` create twice; the second must fail | refuse (`EFSPROBE`); the lease needs it |
| Hard links | link a file | copy instead of link (transaction before-images) |

The result is recorded in `format.json` (`metadata: "xattr"` or
`"sidecar"`, `hardLinks: true|false`). **Every later open** checks that the
recorded way still works (one small probe, a few ms) and refuses with
`EFSPROBE` if it does not, naming what failed (for example an app folder
copied from ext4 to a share without extended attributes). `EFSPROBE` is new,
stable and not retryable.

**ADR 0010's sidecar format becomes automatic.** It is chosen by the probe,
on every OS (not only Unix). The sidecar of a payload is
`.<TTID>.<ext>.fylo-attrs` (for a document, `.<TTID>.json.fylo-attrs`), in
the same folder as its payload, so ADR 0010's crash protocol (the pair is
renamed together) does not change; only the name gains a leading dot so it
is hidden. `--filesystem-profile efs-sidecar-v1` stays only as a
creation-time override for tests (it forces sidecars); on an existing app
folder whose recorded format differs it is refused.

**ADR 0032 is amended** by section 5: the mount-point test is added, the
disk comparison is unchanged, and memory file systems stay refused.

### 7. Files beside documents, linked by id

Q3 and N1.

- **One collection holds documents and files.** The kind of a collection
  (`record` or `file`, today `.collections` or `.buckets`) goes away. The
  bucket API (`putFile`, `getFileData`, `putData` with a `file` field) stays
  and writes into the same collection folder.
- **A file is its own record with its own TTID**: `<TTID>.<original
  extension>`, bytes unchanged. Its metadata (name, key, content type,
  length, checksums, custom `meta`) is in extended attributes or the sidecar
  (section 6), as today.
- **Documents own `.json`.** A file whose extension is `json`, missing, or
  unsafe (not lowercase ASCII letters and digits, 1-16 bytes) is stored as
  `<TTID>.bin`. Its original name stays in its metadata.
- **A document links to a file by storing the file's TTID in a field**
  (N1: "Id stored in a field (Rec.)"), for example `{"photo": "4UUB32VGUDX"}`.
  Many documents may link one file, a document may link many files, and a
  file may exist alone.
  - A field is a link only when the collection's schema (ADR 0017) declares
    it as one, with the format `fylo-file-id`. FYLO cannot guess which
    strings are ids.
  - **On write** FYLO checks that every declared link names a live file in
    the same collection, and refuses the write otherwise (`EFILELINK`, new,
    stable, not retryable).
  - **Deleting a linked file is allowed.** The link then points at nothing;
    `fylo verify` reports such links. A reverse index to refuse those
    deletes can come later, in its own decision.
- **Counting toward "big"** (section 8): documents and files together.

### 8. Shard folders only when a collection is big

Q6: "Shard only when big". N8: "Automatic (Rec.)".

- **Flat** (`<collection>/<TTID>.json`, width 0) up to **10,000 live
  entries** (documents and files) in the collection's top folder. Then width
  1 (36 folders) up to about 360,000, then width 2 (1,296 folders). Width 0,
  removed in 2026-08, comes back on purpose. ADR 0006's rule otherwise holds:
  the shard is the last characters of the TTID's creation segment.
- Lookups never depend on folder size (FYLO builds the path from the id).
  The limit protects listing: `ls`, Finder, Explorer, backup tools, and
  network shares, where a large folder listing is slow. ADR 0006 measured
  that too many small folders costs time too, so small collections stay
  flat.
- The count comes from the collection's index, never a folder walk. The limit
  is a setting (`fylo.shard.flatLimit`), but the width is recorded per
  collection in its descriptor, so two processes never disagree.
- **The move is automatic**, done only by the namespace's owner, with ADR
  0006's resumable `reshardCollection`:
  1. record the new width and the old width in the descriptor; from then on
     new writes go to the new place, and a lookup tries the new place, then
     the old one;
  2. move entries in batches (about 500 renames), each batch under the
     collection's write lock, so a normal write waits at most one batch;
  3. a crash leaves every entry findable in one of the two places; the next
     open finishes the move;
  4. documents keep their bytes; query order does not change;
  5. the handshake and `inspectCollection` report the width and
     `resharding: true` while it runs.
- **FYLO never goes back to a smaller width by itself.** Only `fylo reshard`
  does that, so a collection near the limit does not move back and forth.
- **Tools that read paths** find an entry as `<collection>/<TTID>.json` or
  `<collection>/*/<TTID>.json`, or ask FYLO (`inspectCollection`, or the new
  `fylo path <collection> <id>`). A copy of a live folder during a move can
  miss or duplicate an entry; back up with `fylo pack` or after `fylo
  checkpoint`.

### 9. The temp tier (ADR 0030)

- **Keys are per namespace folder.** The temp tier's root key becomes a hash
  of the namespace folder's canonical path and device (today: the root's).
  So two namespaces never share a cache entry, without adding the namespace
  to each entry key. The tier-3 query-result cache is per process and per
  namespace.
- **`auto` rule 1 becomes "the namespace's file system is a network file
  system"** (N13: "Network file system (Rec.)"), not "opened with
  `--filesystem-profile efs-sidecar-v1`". It is a speed hint only, never a
  refusal. The other `auto` rules do not change. FYLO-S already routes rule
  1 through one function, so L1-8 changes only its source.
- The temp folder stays where it is (`$TMPDIR/fylo-cache-v1`). It is not a
  root, so the drive rule does not apply to it (ADR 0032 section 1).

### 10. `.vdbx` per namespace

Q8: "<drive>/<app>/<namespace>.vdbx (Rec.)".

- A namespace is **either** a folder `<app>/<ns>/` **or** a file
  `<app>/<ns>.vdbx`, never both; when both exist FYLO refuses the namespace
  with `ENAMESPACE`.
- The app's `exec --loop` serves both kinds. A `.vdbx` keeps its own file
  lease, as today (ADR 0016 section 7), and gains the heartbeat and fencing
  of section 4 where its file lock cannot be trusted. The inside of the file
  does not change for this ADR.
- **Files in `.vdbx`.** A namespace with files can be a `.vdbx` once ADR
  0034 (large objects) is built. Until then `fylo pack` refuses a namespace
  with files with `ELAYOUTUNSUPPORTED`, as it refuses buckets today.
- `fylo pack <drive>/<app> --namespace <ns> --out <drive>/<app>/<ns>.vdbx`
  and `fylo unpack <ns>.vdbx --out <drive>/<app> --namespace <ns>`.
- The browser (OPFS, IndexedDB) and mobile keep opening a `.vdbx` by its
  own path; there is no drive there.

### 11. Migration: `fylo upgrade-root`

Q9: "Offline copy command (Rec.)".

- `fylo upgrade-root --from <old root> --root <drive>/<app> [--namespace
  public]` copies an old root into one namespace of the new layout: documents
  and files (old buckets) into the namespace's collections, with their
  metadata, version control, queue and schemas. It checks every document and
  file the way `fylo verify` does, and leaves the old root untouched. It
  refuses to write into an existing namespace.
- Old collection names longer than 64 bytes, or old document and bucket
  collections with the same name, are refused with a clear message before
  anything is copied.
- **Old roots open read-only.** The new binary opens an old-layout root
  (`.collections/` is a folder) for reads and exports, and refuses every
  write with `ELEGACYLAYOUT` (new, stable, not retryable), whose message
  names `fylo upgrade-root`. A later release removes the read-only path; that
  release is marked as a breaking change too.
- Document-only roots can also go through `fylo pack` and then `fylo unpack
  --out <drive>/<app> --namespace <ns>`. The name `migrateLayout` stays for
  ADR 0015's segments operation.

### 12. Tests and CI: real disk images on HANGAR

Q4: "Real disk images". Then the user changed where the images live, quoted
exactly: "actually i've changed my mind, for testing lets create a virtual
disk image that can be used for all FYLO testing instead of ATLAS as ATLAS is
used for production data", with the file on "HANGAR SSD (Rec.)". N17: "One
image per system (Rec.)". This replaces the earlier answers N4 ("On the
ATLAS drive"), N14 and N15 for test images. **Nothing FYLO tests lives on
ATLAS**; ATLAS is for production data.

**It passes the drive rule.** HANGAR is a USB SSD that is not the boot disk,
so an image whose file is on HANGAR passes section 5 with no waiver (ADR
0032 section 2.2 follows an image to its file).

**One file system, one system at a time.** An image that holds one file
system is never attached to two systems at once (the Mac and windows-vm, or
the Mac and Colima): each OS caches the file system in its own memory, and
two writers corrupt it. FYLO's leases work on files, below that cache, so
they cannot prevent it. No file system is native to all three systems (only
exFAT, which has no extended attributes or hard links, so FYLO's normal mode
would never be tested). So each system has **its own reusable test image,
all on HANGAR**:

| System | The test image | Attached by | File system |
| ------ | -------------- | ----------- | ----------- |
| macOS lane (this Mac, jobq jobs, the nightly) | a sparsebundle on HANGAR, in a folder Platform confirms when slice L1-11 is near | the test helper, `hdiutil attach -nobrowse` at a fixed mount point, kept attached between jobs | APFS |
| windows-vm | one reusable sparse raw image `/Volumes/HANGAR/fylo-test/windows-vm.raw` (made by Platform), the VM's second NVMe disk (serial `TEST-DISK`) | **only `testvm`** (Platform's change), at boot, under the `vm-windows` lock | NTFS |
| Linux lane (Colima) | no image file: a named Docker volume (for example `fylo-test-volume`) mounted at `/data` in FYLO's test container. Colima's data disk is itself a file on HANGAR and is not the VM's boot disk, so it passes. No `--privileged` and no loop devices are needed. | Docker | ext4 |
| Laptops away from home, GitHub Linux and Windows runners | a small image the test helper makes on a disk that is not the boot disk (GitHub: `/mnt`, `D:`) | the test helper, per run | native |
| GitHub macOS runners | one disk only | - | **open**; to be asked again when the CI slice (L1-11) is near |

Rules that keep each image safe:

1. **Each image belongs to exactly one system.** The Mac never attaches the
   windows-vm image, and the other way round. The test helper refuses to
   attach any image under the test-disk folders except the Mac lane's own,
   and checks `hdiutil info` so it never attaches it twice.
2. **The Mac image is attached and detached under one lock**
   (`~/Library/Caches/mac-locks/fylo-test-image`). A write-and-delete probe
   runs after every attach and before every job, because an attach can be
   read-only and still return 0; never trust the exit code alone.
3. **Many jobs share one system's image at once** without waiting for each
   other: each test makes its own root `<mount>/t-<job id>-<test id>`
   (directly below the mount point, a valid app name), so no two jobs share
   a root or a lease. Tests that need namespaces use them inside their own
   app.
4. **Cleanup.** Each job deletes its own `t-<job id>-*` folders on every exit
   path, failures included. At the start of each job a sweep deletes `t-*`
   folders older than 24 hours whose leases are not held (leftovers of
   crashed jobs).
5. **Space.** Each image is sparse with a maximum of **8 GiB**, so the Mac
   and windows-vm images together never take more than 16 GiB of HANGAR. The
   helper refuses to start a job when the image has under 1 GiB free inside.
   Freed space goes back to HANGAR: `hdiutil compact` on the Mac image about
   once a week, when no job holds it; Windows trim and `testvm`'s
   `discard=unmap` for the windows-vm image.

Attach only inside `mac-heavy`/jobq jobs; the Bash tool's sandbox cannot be
relied on. The nightly run uses the same images.

The ADR 0032 opt-out stays available for tests of other projects (TACHYON's
temporary folders) and for tests of the drive rule itself. Tests of the rule
clear it, as ADR 0032 section 7 does.

**The macOS image before L1 ships** (FYLO-T1, 2026-11-21). The Mac lane's
image exists before this ADR is built, so tests stop using the system disk
now:

- `scripts/ci/test-image.sh` makes and attaches
  `/Volumes/HANGAR/fylo-test/fylo-macos-test.sparsebundle` (8 GiB maximum)
  at `~/Developer/FyloTestImage`, under the lock named in rule 2.
  `run -- <command>` gives each job the folder
  `<mount>/roots/<owner>-<pid>` as `FYLO_TEST_ROOT` and removes it at the
  end. Rust tests read it through `fylo_test_support::TestFolder::path()`,
  bun tests through `testFolder()` in `tests/helpers/root.js`; both use the
  OS temp folder when it is unset (GitHub CI, the VMs).
- These roots are not directly below the mount point. That is enough under
  ADR 0032, which checks only the disk. When the drive rule ships, the L1
  slice for tests replaces this shape: each job gets one app folder
  `<mount>/<app>` and each test its own namespace in it (Q2b: "for testing
  we can use namespaces feature for the slices").
- Measured on 2026-11-21 (develop 1ce4115 plus this change, 1,682 Rust
  tests): at most 291 MiB was in use inside the image during a full
  `cargo nextest` run, so 8 GiB is ample. The image file grew from 15 MB to
  585 MB over three full runs, because freed blocks stay in the file until
  `compact`; one compact took it back to 292 MB. The full Rust suite took
  420 s and 559 s on the image against 318 s and 329 s in TMPDIR (two
  samples, 2 and 3 test threads); the bun interop suite 57 s against 47 s.
- The opt-out is no longer needed to open test roots on the image: with
  `FYLO_ALLOW_SYSTEM_DISK=0`, all 287 interop and 202 browser tests and 1,673
  of 1,682 Rust tests pass. The other 9 Rust tests check the opt-out itself
  (7 expect `placement: "systemDiskAllowed"` in the handshake, 2 expect the
  test process to have it on), so the repository keeps it on until those
  tests change.

### 13. Versioning: how the break is marked

FYLO uses CalVer (`YY.WW.DD`) and has no major number. As in ADR 0032
section 8, the break is marked by:

- a `### Breaking Changes` section first in the CHANGELOG and the release
  notes of the layout release, with the migration note below;
- new stable error codes: `EAPPNAME`, `ENAMESPACE`, `ENOTVOLUMEROOT`,
  `ENOTFYLOROOT`, `EFSPROBE`, `EFILELINK`, `ELEGACYLAYOUT`; `EREMOTEROOT` is
  retired;
- the **storage contract goes to `v2`** (`api/storage/v2/`, drafted with this
  ADR): `format.json`, the namespace lease record and the namespace catalog;
- the **machine protocol stays `v1`**: additive only (the optional
  `namespace` field, three new operations, new handshake fields, new error
  codes), as ADR 0032 section 8 did;
- the library API (`NativeRoot`, `RootConfig`) changes: opens take a
  namespace.

**Handshake** (`storage`): `appName` (the folder name or `null`), `volume`
(the mount point), `layout` (`"v2"` or `"legacy-read-only"`), `namespace`
(the default for this process), `metadata` (`"xattr"` or `"sidecar"`) and
`remote` (the file system is a network one). `rootLock` keeps `systemDisk`
and `placement`. A plain `exec --loop` handshake reports these without
opening the app folder, so a host can check a root before use.

**Release.** This ships in its own release after the next FYLO release (the
S5 release), never together with it. TACHYON and Mesail test against a
`develop` build first (the release rule): their work starts when L1-5 is on
`develop`.

## Migration note

Who must act before upgrading:

1. **Every persistent root.** Stop the processes that use it. Then:
   `fylo upgrade-root --from <old root> --root <drive>/<app> --namespace
   public`, check the result, and point `--root` (or `fylo.root`, or the
   `fylo://` URL) at `<drive>/<app>`. The old root stays as it was; keep it
   until the new one is checked. Document-only roots may use `fylo pack`
   then `fylo unpack --out <drive>/<app> --namespace public` instead.
2. **Roots that are not directly below a mount point.** Move the app folder
   to the drive's top folder. Several roots that were sibling folders of one
   parent can become namespaces of one app.
3. **Scripts that delete root folders** (`rm -rf`): use `fylo namespace
   drop`, which refuses while the namespace is in use.
4. **Tools that read FYLO's paths** (`.collections/<c>/docs/<shard>/...`):
   read `<app>/<ns>/<c>/<TTID>.json` or `<app>/<ns>/<c>/*/<TTID>.json`, or
   ask FYLO.
5. **Backups that copy folders:** run `fylo checkpoint` first, or use `fylo
   pack`. The lease files are now inside the app folder; copy the app folder
   only while no process owns its namespaces.
6. **Network roots** that ADR 0029 refused now open. Nothing to do; `fylo
   upgrade-root` probes the file system when it creates the app folder.
7. **Tests:** temporary-folder roots keep working with the ADR 0032 opt-out
   (`FYLO_ALLOW_SYSTEM_DISK=1`).

Known users on 2026-11-21 (their work is slice L1-13, in their own sessions,
after L1-5 is on `develop`):

- **TACHYON** (TAC-FL1): a datasource `namespace` setting
  (`fylo://<drive>/<app>?namespace=<ns>`, passed as `--namespace`); its URL
  and exec arguments otherwise do not change. The test
  `crates/tachyon-core/src/storage/tests/fylo_documents.rs` stops reading
  `.collections/hd-events/docs`. Its tests keep `--allow-system-disk`. No
  `efs-sidecar-v1` setting is needed any more.
- **Mesail:** gate roots move from `<image>/slice-<id>/<app>` to
  `<image>/<app>` with the namespace `slice-<id>`; native runs use
  `<owner>-native`; slice and owner ids must fit the name rule; `gates.sh`
  and `scripts/native/run-all.sh` use `fylo namespace drop`; persistent
  dev-host roots go through `fylo upgrade-root`. Lambda roots
  (`/mnt/data/<service>`) and `realtime` (`/data/<app>`) keep their form and
  use `public`. The MS1048 capture tars `<app>/` (the lease files are inside
  it now).

## CHANGELOG entry (draft)

For the layout release, after the S5 release. Not added to `CHANGELOG.md`
yet, because its `Unreleased` section is the S5 release.

```markdown
### Breaking Changes

- **New on-disk layout: `<drive>/<app>/<namespace>/<collection>/<TTID>.json`
  (ADR 0033).** The root is an app folder directly in the top folder of a
  mounted drive that is not the boot disk; its folder name is the app name
  (2-64 lowercase letters, digits or `-`). Inside it, namespaces work like
  Postgres schemas (default `public`): each request may name one
  (`"namespace"`, or `--namespace` for the default), and SQL accepts
  `namespace.collection`. Documents are `<TTID>.json` and files (images,
  videos, any bytes) are `<TTID>.<ext>` in the same collection folder;
  derived files are in a hidden `.fylo/` folder per collection. A
  collection gets shard folders by itself once it holds 10,000 entries.
  New codes: `EAPPNAME`, `ENAMESPACE`, `ENOTVOLUMEROOT`, `ENOTFYLOROOT`,
  `EFSPROBE`, `EFILELINK`, `ELEGACYLAYOUT`. Old roots open read-only; copy
  them with `fylo upgrade-root`. Older FYLO binaries refuse the new layout.
  See the migration note in ADR 0033.
- **One lease per namespace, safe on any file system.** Two processes can
  own two namespaces of one app. The lease files are
  `<app>/.<namespace>.fylo-root-owner.lock(.json)`. The owner writes a
  heartbeat every 5 s and checks a fencing number before every commit;
  after a crash the next owner waits up to 20 s. New operations
  `createNamespace`, `dropNamespace`, `listNamespaces`, and
  `fylo namespace create|drop|list`.
- **Network file systems are admitted (ADR 0029 replaced).** FYLO no longer
  refuses a root by file-system type, except memory file systems. It tests
  the file system once when it creates an app folder and picks extended
  attributes or hidden sidecar files by itself; `EREMOTEROOT` is retired.
- **Storage contract `v2`.** `api/storage/v2/` describes `format.json`, the
  namespace lease record and the namespace catalog. The machine protocol
  stays `v1` (additive fields and operations).
- **The handshake reports the layout.** `storage` gains `appName`,
  `volume`, `layout`, `namespace`, `metadata` and `remote`.
```

## Consequences

- The visible tree is the user's format: a person or a backup tool sees
  apps, namespaces, collections and `<TTID>.json` files, with images and
  videos beside them.
- One process per app still works (TACHYON, Mesail Lambdas), and parallel
  test slices work as namespaces of one app.
- Network shares become usable roots. The price is a heartbeat write every
  5 s, one small read per commit, and a wait of up to 20 s after a crash.
- Every root is checked once at creation, so a file system that cannot do
  atomic renames or exclusive creates is refused with a clear reason instead
  of failing later.
- Paths change once for a collection when it reaches 10,000 entries. Tools
  must allow for both shapes or ask FYLO.
- Tests need a real mounted image, which costs a few seconds per job and
  needs the rules of section 12. windows-vm waits for Platform's `testvm`
  change that adds its second disk from HANGAR.
- The break needs a migration of every persistent root, and changes in
  TACHYON and Mesail (L1-13).
- Two layouts are read by one release (the new one, and the old one
  read-only), until a later release removes the old read path.

## Alternatives considered

- **The drive's top folder as the root.** Its lease files would have no
  writable parent on macOS, Windows and many containers. Rejected.
- **An `--app` flag** (the user's first Q5 answer). It would give the app
  name twice next to `--root`, and change TACHYON's and Mesail's arguments.
  The user replaced it with "Folder name = app".
- **One lease per app.** Simpler, and cross-namespace reads always work, but
  Mesail's parallel gate slices would collide. The user chose one lock per
  namespace (N9).
- **"The OS reports the disk as external"** (N16 B) or a mixed rule (N16 C).
  Every root inside VMs, containers, cloud servers and CI runners would be
  refused, including Mesail's whole deployment. The user chose "Not the boot
  disk".
- **One mode for every file system** (sidecars everywhere, N12 B). One code
  path, but local disks would pay two files per item with metadata. The user
  chose the probe.
- **The literal `<collection>/<TTID>.json` with no shard ever.** A collection
  with millions of entries would be one folder, slow to list on network
  shares. The user chose "Shard only when big".
- **Changing roots in place on first open.** Fast, but a crash in the middle
  needs its own recovery journal. The user chose the offline copy.

## Verification

Each slice proves its part, in its own gates:

- **L1-1 drive rule:** admitted and refused cases of section 5.1 on macOS
  (the test image on HANGAR, the boot disk, an image whose file is on the
  boot disk, a nested folder), Linux (a Docker volume on
  the VM's data disk, `tmpfs`, overlay) and Windows; the opt-out waives every
  check and still prints its warning; app mode admits the OS app folder.
- **L1-2 names:** every reason of `EAPPNAME` and `ENAMESPACE`; Windows
  device names; the guard files make an old binary fail without writing.
- **L1-3 lease:** a crash matrix of the takeover (kill at every step); two
  processes on two namespaces at once; a paused owner stops with
  `EROOTLEASELOST` before it writes; the takeover on an NFS mount where the
  kernel lock binds one machine only (the 2026-11-12 Mesail case).
- **L1-4 probe:** attributes chosen on APFS, ext4 and NTFS; sidecars chosen
  where attributes fail; `EFSPROBE` when a copied app folder loses them.
- **L1-5 to L1-8:** the vertical path, files and links, the automatic
  reshard under writes with a crash in the middle, and two namespaces that
  never share a temp-tier entry.
- **L1-10 migration:** `fylo upgrade-root` on the 5k fixture with buckets,
  version control and a queue, then `fylo verify`; `ELEGACYLAYOUT` on a write
  to an old root; an old binary refuses a new app folder.
- **L1-11 tests:** one attach test per system, with Platform; the helper
  refuses to attach another system's image or the same image twice; two
  jobs on one image at once; the 24-hour sweep leaves held roots alone.

## Review

- Decisions: the user, 2026-11-21, five rounds through the hub (quoted in
  "Context").
- Changed after the first merge, the same day: section 12 now puts test
  images on HANGAR, one per system (the user's change of mind and N17), in
  place of ATLAS. The decision was not built yet.
- Reviewed by the hub on 2026-11-21: details 1-5 accepted; detail 5 with the
  key/nonce rule of ADR 0034 section 4 (a key is never used twice with the
  same nonce). The details are section 4's claim file, section 7's schema
  declaration of links and `EFILELINK`, and the new codes `ENOTFYLOROOT` and
  `EFSPROBE` (this ADR), and those listed in ADR 0034's review.
