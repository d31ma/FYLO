import { CargoArtifacts } from './cargo-artifacts.mjs'
import { access, copyFile, mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises'
import { platform, tmpdir } from 'node:os'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

import { Fylo as MachineFylo } from '../clients/node/fylo.mjs'

const repo = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const workspace = await mkdtemp(join(tmpdir(), 'fylo-rust-clients-'))
const collection = 'users'
const secretSentinel = 'FYLO_SECRET_SENTINEL_MUST_NOT_LEAK'

const rustBinary = process.env.FYLO_RUST_BINARY
    ? resolve(process.env.FYLO_RUST_BINARY)
    : CargoArtifacts.path('debug', platform() === 'win32' ? 'fylo-rust.exe' : 'fylo-rust')

try {
    if (process.env.FYLO_SKIP_RUST_BUILD !== '1') {
        await command([
            process.execPath,
            './scripts/run-rust.mjs',
            'cargo',
            'build',
            '--locked',
            '-p',
            'fylo-cli',
            '--bin',
            'fylo-rust'
        ])
    }

    const clients = await clientProbes(workspace)
    const required = new Set(
        (process.env.FYLO_REQUIRED_LANGUAGE_CLIENTS ?? 'node,python,ruby,php,go,java,rust')
            .split(',')
            .map((name) => name.trim())
            .filter(Boolean)
    )
    const available = []
    for (const client of clients) {
        if (!(await commandExists(client.command))) {
            if (required.has(client.name)) {
                throw new Error(`required language client tool is unavailable: ${client.command}`)
            }
            console.log(`Skipping ${client.name}: ${client.command} is unavailable`)
            continue
        }
        available.push(client)
    }
    for (const requiredName of required) {
        if (!available.some((client) => client.name === requiredName)) {
            throw new Error(`required language client probe is unavailable: ${requiredName}`)
        }
    }

    for (const client of available) {
        const root = join(workspace, `${client.name}-rust`)
        const defaultRoot = join(workspace, `${client.name}-default-rust`)
        const configurationFile = join(root, 'application.json')
        const defaultConfiguration = join(defaultRoot, 'application.json')
        const invalidConfiguration = join(root, 'invalid-application.json')
        const schema = join(root, 'schema')
        await mkdir(root, { recursive: true })
        await mkdir(defaultRoot, { recursive: true })
        await mkdir(join(schema, collection, 'history'), { recursive: true })
        await writeFile(
            join(schema, collection, 'manifest.json'),
            JSON.stringify({ current: 'v1' })
        )
        await writeFile(
            join(schema, collection, 'history', 'v1.schema.json'),
            JSON.stringify({
                type: 'object',
                $encrypted: ['name']
            })
        )
        await writeFile(
            configurationFile,
            JSON.stringify({
                fylo: {
                    schema,
                    shard: { width: 2 },
                    encryption: { key: '${FYLO_LANGUAGE_TEST_KEY}' },
                    cipher: { salt: '${FYLO_LANGUAGE_TEST_SALT}' }
                }
            })
        )
        await writeFile(
            join(root, '.env'),
            'FYLO_LANGUAGE_TEST_KEY=interop-secret-at-least-thirty-two-bytes\nFYLO_LANGUAGE_TEST_SALT=interop-cipher-salt\n'
        )
        await writeFile(defaultConfiguration, JSON.stringify({ fylo: {} }))
        await writeFile(
            invalidConfiguration,
            JSON.stringify({ fylo: { shard: { width: secretSentinel } } })
        )
        const identifier = await seed(root, rustBinary, configurationFile)
        const reviewer = `${client.name}-rust`
        const inheritedEnvironment = {
            [platform() === 'win32' ? 'fylo_shard_width' : 'FYLO_SHARD_WIDTH']: '4'
        }
        const output = (
            await client.run(
                rustBinary,
                root,
                reviewer,
                identifier,
                configurationFile,
                inheritedEnvironment
            )
        ).trim()
        const created = output.startsWith('{') ? JSON.parse(output).result : output
        if (!created)
            throw new Error(`${client.name} did not return its configuration-scoped document`)
        await access(
            join(root, '.collections', collection, 'docs', created.slice(-2), `${created}.json`)
        )
        await verify(root, rustBinary, reviewer, identifier, configurationFile)
        let invalidRejected = false
        try {
            await client.run(
                rustBinary,
                root,
                reviewer,
                identifier,
                invalidConfiguration,
                inheritedEnvironment
            )
        } catch (error) {
            if (String(error).includes(secretSentinel))
                throw new Error(`${client.name} leaked an invalid setting value`)
            invalidRejected = true
        }
        if (!invalidRejected)
            throw new Error(`${client.name} accepted invalid application configuration`)
        const defaultOutput = (
            await client.run(
                rustBinary,
                defaultRoot,
                reviewer,
                '',
                defaultConfiguration,
                inheritedEnvironment
            )
        ).trim()
        const defaultIdentifier = defaultOutput.startsWith('{')
            ? JSON.parse(defaultOutput).result
            : defaultOutput
        if (!defaultIdentifier) {
            throw new Error(`${client.name} did not return its default-configuration document`)
        }
        await access(
            join(
                defaultRoot,
                '.collections',
                collection,
                'docs',
                defaultIdentifier.slice(-1),
                `${defaultIdentifier}.json`
            )
        )
    }
    console.log(`Verified ${available.length} published language clients against the Rust engine`)
} finally {
    await rm(workspace, { recursive: true, force: true })
}

async function seed(root, binary, config) {
    await mkdir(root, { recursive: true })
    const db = new MachineFylo(root, { binary, config, exclusiveRoot: true })
    try {
        await db.ready
        const create = await db.request({
            op: 'createCollection',
            collection,
            kind: 'document'
        })
        if (!create.ok) {
            throw new Error(
                `createCollection seed failed (${create.error?.code}): ${create.error?.message}`
            )
        }
        const put = await db.request({
            op: 'putData',
            collection,
            data: { name: 'Ada', role: 'admin' }
        })
        if (!put.ok) {
            throw new Error(`putData seed failed (${put.error?.code}): ${put.error?.message}`)
        }
        return put.result
    } finally {
        await db.close()
    }
}

async function verify(root, binary, reviewer, identifier, config) {
    const db = new MachineFylo(root, { binary, config, exclusiveRoot: true })
    await db.ready
    const metadata = await db.getMeta(collection, identifier)
    if (metadata.reviewer !== reviewer || metadata.id !== identifier) {
        throw new Error(`client metadata did not round-trip: ${reviewer}`)
    }
    await db.close()
}

async function clientProbes(root) {
    const probes = []
    probes.push(await nodeProbe(root))
    probes.push(await pythonProbe(root))
    probes.push(await rubyProbe(root))
    probes.push(await phpProbe(root))
    probes.push(await goProbe(root))
    probes.push(await javaProbe(root))
    probes.push(await rustProbe(root))
    probes.push(await csharpProbe(root))
    probes.push(await dartProbe(root))
    return probes
}

async function nodeProbe(root) {
    const source = join(root, 'node-probe.mjs')
    await writeFile(
        source,
        `import { Fylo } from ${JSON.stringify(resolve('clients/node/fylo.mjs'))}
const [binary, root, reviewer, id, config] = process.argv.slice(2)
const db = new Fylo(root, { binary, config })
try {
  if (!id) {
    await db.createCollection('users')
    console.log(await db.putData('users', { configuration: 'node-default' }))
    process.exitCode = 0
  } else {
  const get = await db.request({ op: 'getLatest', collection: 'users', id })
  const find = await db.request({ op: 'findDocs', collection: 'users', query: { $ops: [{ role: { $eq: 'admin' } }] } })
  const meta = await db.request({ op: 'setMeta', collection: 'users', id, meta: { reviewer } })
  if (get.result[id].name !== 'Ada' || Object.keys(find.result).length !== 1 || meta.result.reviewer !== reviewer) throw new Error('node client corpus mismatch: ' + JSON.stringify({ get, find, meta }))
  console.log(await db.putData('users', { configuration: 'node' }))
  }
} finally { await db.close() }
`
    )
    return probe('node', 'node', (binary, database, reviewer, identifier, config, environment) =>
        command(['node', source, binary, database, reviewer, identifier, config], {
            env: environment
        })
    )
}

async function pythonProbe(root) {
    const source = join(root, 'python_probe.py')
    await writeFile(
        source,
        `import sys
sys.path.insert(0, ${JSON.stringify(resolve('clients/python'))})
from fylo import Fylo
binary, root, reviewer, identifier, config = sys.argv[1:]
with Fylo(root, binary=binary, config=config) as db:
    if not identifier:
        db.create_collection('users')
        print(db.put_data('users', {'configuration': 'python-default'}))
        sys.exit(0)
    get = db.request({'op':'getLatest','collection':'users','id':identifier})
    find = db.request({'op':'findDocs','collection':'users','query':{'$ops':[{'role':{'$eq':'admin'}}]}})
    meta = db.request({'op':'setMeta','collection':'users','id':identifier,'meta':{'reviewer':reviewer}})
    assert get['result'][identifier]['name'] == 'Ada'
    assert len(find['result']) == 1
    assert meta['result']['reviewer'] == reviewer
    print(db.put_data('users', {'configuration': 'python'}))
`
    )
    return probe(
        'python',
        'python3',
        (binary, database, reviewer, identifier, config, environment) =>
            command(['python3', source, binary, database, reviewer, identifier, config], {
                env: environment
            })
    )
}

async function rubyProbe(root) {
    const source = join(root, 'ruby_probe.rb')
    await writeFile(
        source,
        `require ${JSON.stringify(resolve('clients/ruby/fylo.rb'))}
binary, root, reviewer, identifier, config = ARGV
Fylo.open(root, binary: binary, config: config) do |db|
  if identifier.empty?
    db.create_collection('users')
    puts db.put_data('users', { 'configuration' => 'ruby-default' })
    next
  end
  get = db.request({'op'=>'getLatest','collection'=>'users','id'=>identifier})
  find = db.request({'op'=>'findDocs','collection'=>'users','query'=>{'$ops'=>[{'role'=>{'$eq'=>'admin'}}]}})
  meta = db.request({'op'=>'setMeta','collection'=>'users','id'=>identifier,'meta'=>{'reviewer'=>reviewer}})
  raise 'ruby get mismatch' unless get['result'][identifier]['name'] == 'Ada'
  raise 'ruby find mismatch' unless find['result'].length == 1
  raise 'ruby metadata mismatch' unless meta['result']['reviewer'] == reviewer
  puts db.put_data('users', { 'configuration' => 'ruby' })
end
`
    )
    return probe('ruby', 'ruby', (binary, database, reviewer, identifier, config, environment) =>
        command(['ruby', source, binary, database, reviewer, identifier, config], {
            env: environment
        })
    )
}

async function phpProbe(root) {
    const source = join(root, 'php_probe.php')
    await writeFile(
        source,
        `<?php
require ${JSON.stringify(resolve('clients/php/fylo.php'))};
[$script, $binary, $root, $reviewer, $identifier, $config] = $argv;
$db = new Fylo($root, $binary, $config);
try {
  if ($identifier === '') {
    $db->createCollection('users');
    echo $db->putData('users', ['configuration'=>'php-default']);
    exit(0);
  }
  $get = $db->request(['op'=>'getLatest','collection'=>'users','id'=>$identifier]);
  $find = $db->request(['op'=>'findDocs','collection'=>'users','query'=>['$ops'=>[['role'=>['$eq'=>'admin']]]]]);
  $meta = $db->request(['op'=>'setMeta','collection'=>'users','id'=>$identifier,'meta'=>['reviewer'=>$reviewer]]);
  if ($get['result'][$identifier]['name'] !== 'Ada' || count($find['result']) !== 1 || $meta['result']['reviewer'] !== $reviewer) throw new Exception('php client corpus mismatch');
  echo $db->putData('users', ['configuration'=>'php']);
} finally { $db->close(); }
`
    )
    return probe('php', 'php', (binary, database, reviewer, identifier, config, environment) =>
        command(['php', source, binary, database, reviewer, identifier, config], {
            env: environment
        })
    )
}

async function goProbe(root) {
    const directory = join(root, 'go')
    await mkdir(join(directory, 'fylo'), { recursive: true })
    await copyFile(resolve('clients/go/fylo.go'), join(directory, 'fylo', 'fylo.go'))
    await writeFile(join(directory, 'go.mod'), 'module fylo-probe\n\ngo 1.24\n')
    await writeFile(
        join(directory, 'main.go'),
        `package main
import ("fmt"; "os"; fylo "fylo-probe/fylo")
func main() {
  db, err := fylo.OpenWithOptions(os.Args[2], fylo.Options{Binary: os.Args[1], Config: os.Args[5]}); if err != nil { panic(err) }; defer db.Close()
  if os.Args[4] == "" {
    if _, err := db.CreateCollection("users", "document"); err != nil { panic(err) }
    created, err := db.PutData("users", map[string]any{"configuration":"go-default"}); if err != nil { panic(err) }
    fmt.Print(created.(string)); return
  }
  get, err := db.Request(map[string]any{"op":"getLatest","collection":"users","id":os.Args[4]}); if err != nil { panic(err) }
  find, err := db.Request(map[string]any{"op":"findDocs","collection":"users","query":map[string]any{"$ops":[]any{map[string]any{"role":map[string]any{"$eq":"admin"}}}}}); if err != nil { panic(err) }
  meta, err := db.Request(map[string]any{"op":"setMeta","collection":"users","id":os.Args[4],"meta":map[string]any{"reviewer":os.Args[3]}}); if err != nil { panic(err) }
  doc := get["result"].(map[string]any)[os.Args[4]].(map[string]any)
  if doc["name"] != "Ada" || len(find["result"].(map[string]any)) != 1 || meta["result"].(map[string]any)["reviewer"] != os.Args[3] { panic("go client corpus mismatch") }
  created, err := db.PutData("users", map[string]any{"configuration":"go"}); if err != nil { panic(err) }
  fmt.Print(created.(string))
}
`
    )
    return probe('go', 'go', (binary, database, reviewer, identifier, config, environment) =>
        command(['go', 'run', '.', binary, database, reviewer, identifier, config], {
            cwd: directory,
            env: environment
        })
    )
}

async function javaProbe(root) {
    const directory = join(root, 'java')
    await mkdir(directory, { recursive: true })
    await copyFile(resolve('clients/java/Fylo.java'), join(directory, 'Fylo.java'))
    await writeFile(
        join(directory, 'Probe.java'),
        `public final class Probe {
  public static void main(String[] args) throws Exception {
    Fylo.Options options = new Fylo.Options();
    options.binary = args[0];
    options.config = args[4];
    try (Fylo db = new Fylo(args[1], options)) {
      if (args[3].isEmpty()) {
        db.createCollection("users");
        System.out.print(db.putData("users", java.util.Map.of("configuration", "java-default")));
        return;
      }
      String get = db.request("{\\"op\\":\\"getLatest\\",\\"collection\\":\\"users\\",\\"id\\":\\"" + args[3] + "\\"}");
      String find = db.request("{\\"op\\":\\"findDocs\\",\\"collection\\":\\"users\\",\\"query\\":{\\"$ops\\":[{\\"role\\":{\\"$eq\\":\\"admin\\"}}]}}");
      String meta = db.request("{\\"op\\":\\"setMeta\\",\\"collection\\":\\"users\\",\\"id\\":\\"" + args[3] + "\\",\\"meta\\":{\\"reviewer\\":\\"" + args[2] + "\\"}}");
      if (!get.contains("\\"name\\":\\"Ada\\"") || !find.contains(args[3]) || !meta.contains("\\"reviewer\\":\\"" + args[2] + "\\"")) throw new IllegalStateException("java client corpus mismatch");
      System.out.print(db.putData("users", java.util.Map.of("configuration", "java")));
    }
  }
}
`
    )
    await command(['javac', 'Fylo.java', 'Probe.java'], { cwd: directory })
    return probe('java', 'java', (binary, database, reviewer, identifier, config, environment) =>
        command(
            ['java', '-cp', directory, 'Probe', binary, database, reviewer, identifier, config],
            {
                env: environment
            }
        )
    )
}

async function rustProbe(root) {
    const directory = join(root, 'rust')
    await mkdir(directory, { recursive: true })
    await copyFile(resolve('clients/rust/fylo.rs'), join(directory, 'fylo.rs'))
    await writeFile(
        join(directory, 'probe.rs'),
        `mod fylo;
use fylo::{Fylo, FyloOptions, Json};
fn main() {
  let args: Vec<String> = std::env::args().collect();
  let mut db = Fylo::open_with_options(&args[2], FyloOptions { binary: args[1].clone(), config: Some(args[5].clone().into()) }).unwrap();
  if args[4].is_empty() {
    db.create_collection("users", "document").unwrap();
    print!("{}", db.put_data("users", Json::obj(vec![("configuration", "rust-default".into())])).unwrap());
    return;
  }
  let get = db.request(&format!(r#"{{"op":"getLatest","collection":"users","id":"{}"}}"#, args[4])).unwrap();
  let find = db.request(r#"{"op":"findDocs","collection":"users","query":{"$ops":[{"role":{"$eq":"admin"}}]}}"#).unwrap();
  let meta = db.request(&format!(r#"{{"op":"setMeta","collection":"users","id":"{}","meta":{{"reviewer":"{}"}}}}"#, args[4], args[3])).unwrap();
  assert!(get.contains(r#""name":"Ada""#) && find.contains(&args[4]) && meta.contains(&format!(r#""reviewer":"{}""#, args[3])));
  print!("{}", db.put_data("users", Json::obj(vec![("configuration", "rust".into())])).unwrap());
}
`
    )
    const executable = join(directory, platform() === 'win32' ? 'probe.exe' : 'probe')
    await command(['rustc', 'probe.rs', '-o', executable], { cwd: directory })
    return probe('rust', 'rustc', (binary, database, reviewer, identifier, config, environment) =>
        command([executable, binary, database, reviewer, identifier, config], { env: environment })
    )
}

async function csharpProbe(root) {
    const directory = join(root, 'csharp')
    await mkdir(directory, { recursive: true })
    await copyFile(resolve('clients/csharp/Fylo.cs'), join(directory, 'Fylo.cs'))
    await writeFile(
        join(directory, 'Probe.csproj'),
        '<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup><OutputType>Exe</OutputType><TargetFramework>net8.0</TargetFramework><ImplicitUsings>enable</ImplicitUsings></PropertyGroup></Project>'
    )
    await writeFile(
        join(directory, 'Program.cs'),
        `using Fylo;
using var db = new Fylo.Fylo(args[1], new Fylo.Fylo.Options { Binary = args[0], Config = args[4] });
if (string.IsNullOrEmpty(args[3])) {
    db.CreateCollection("users");
    Console.Write(db.PutData("users", new Dictionary<string, object> { ["configuration"] = "csharp-default" }).GetString());
    return;
}
using var get = db.Request($"{{\\"op\\":\\"getLatest\\",\\"collection\\":\\"users\\",\\"id\\":\\"{args[3]}\\"}}");
using var find = db.Request("{\\"op\\":\\"findDocs\\",\\"collection\\":\\"users\\",\\"query\\":{\\"$ops\\":[{\\"role\\":{\\"$eq\\":\\"admin\\"}}]}}");
using var meta = db.Request($"{{\\"op\\":\\"setMeta\\",\\"collection\\":\\"users\\",\\"id\\":\\"{args[3]}\\",\\"meta\\":{{\\"reviewer\\":\\"{args[2]}\\"}}}}");
if (get.RootElement.GetProperty("result").GetProperty(args[3]).GetProperty("name").GetString() != "Ada" || !find.RootElement.GetProperty("result").TryGetProperty(args[3], out _) || meta.RootElement.GetProperty("result").GetProperty("reviewer").GetString() != args[2]) throw new Exception("csharp client corpus mismatch");
Console.Write(db.PutData("users", new Dictionary<string, object> { ["configuration"] = "csharp" }).GetString());
`
    )
    return probe(
        'csharp',
        'dotnet',
        (binary, database, reviewer, identifier, config, environment) =>
            command(
                [
                    'dotnet',
                    'run',
                    '--project',
                    directory,
                    '--',
                    binary,
                    database,
                    reviewer,
                    identifier,
                    config
                ],
                { env: environment }
            )
    )
}

async function dartProbe(root) {
    const directory = join(root, 'dart')
    await mkdir(directory, { recursive: true })
    await copyFile(resolve('clients/dart/fylo.dart'), join(directory, 'fylo.dart'))
    await writeFile(
        join(directory, 'probe.dart'),
        `import 'dart:io';
import 'fylo.dart';
Future<void> main(List<String> args) async {
  final db = await Fylo.open(args[1], binary: args[0], config: args[4]);
  try {
    if (args[3].isEmpty) {
      await db.createCollection('users');
      stdout.write(await db.putData('users', {'configuration':'dart-default'}));
      return;
    }
    final get = await db.request({'op':'getLatest','collection':'users','id':args[3]});
    final find = await db.request({'op':'findDocs','collection':'users','query':{r'$ops':[{'role':{r'$eq':'admin'}}]}});
    final meta = await db.request({'op':'setMeta','collection':'users','id':args[3],'meta':{'reviewer':args[2]}});
    if (get['result'][args[3]]['name'] != 'Ada' || find['result'].length != 1 || meta['result']['reviewer'] != args[2]) throw StateError('dart client corpus mismatch');
    stdout.write(await db.putData('users', {'configuration':'dart'}));
  } finally { await db.close(); }
}
`
    )
    return probe('dart', 'dart', (binary, database, reviewer, identifier, config, environment) =>
        command(
            [
                'dart',
                'run',
                sourcePath(directory, 'probe.dart'),
                binary,
                database,
                reviewer,
                identifier,
                config
            ],
            { env: environment }
        )
    )
}

function sourcePath(directory, name) {
    return join(directory, name)
}

function probe(name, commandName, run) {
    return { name, command: commandName, run }
}

async function commandExists(name) {
    return Boolean(Bun.which(name))
}

async function command(arguments_, options = {}) {
    const process = Bun.spawn(arguments_, {
        cwd: options.cwd ?? repo,
        env: {
            ...globalThis.process.env,
            DOTNET_CLI_TELEMETRY_OPTOUT: '1',
            DOTNET_NOLOGO: '1',
            DOTNET_GENERATE_ASPNET_CERTIFICATE: 'false',
            PYTHONDONTWRITEBYTECODE: '1',
            ...options.env
        },
        stdout: 'pipe',
        stderr: 'pipe'
    })
    const timeout = setTimeout(() => process.kill(), options.timeout ?? 120_000)
    try {
        const [stdout, stderr, exitCode] = await Promise.all([
            new Response(process.stdout).text(),
            new Response(process.stderr).text(),
            process.exited
        ])
        if (exitCode !== 0) {
            throw new Error(
                `${arguments_.join(' ')} failed (${exitCode})\n${stderr.trim()}\n${stdout.trim()}`
            )
        }
        return stdout
    } finally {
        clearTimeout(timeout)
    }
}
