import { afterAll, beforeAll, describe, expect, test } from 'bun:test'
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'

const roots = []
const workspaces = []
const repoRoot = process.cwd()
const binaryPath = path.join(
    repoRoot,
    'dist-bin',
    process.platform === 'win32' ? 'fylo.exe' : 'fylo'
)

/**
 * @param {string[]} args
 * @param {{ cwd?: string, stdin?: string, timeout?: number }} [options]
 */
async function run(args, options = {}) {
    const proc = Bun.spawn(args, {
        cwd: options.cwd ?? repoRoot,
        stdin: options.stdin === undefined ? 'ignore' : new Blob([options.stdin]),
        stdout: 'pipe',
        stderr: 'pipe'
    })
    const timeout = setTimeout(() => proc.kill(), options.timeout ?? 60_000)
    try {
        const [stdout, stderr, exitCode] = await Promise.all([
            new Response(proc.stdout).text(),
            new Response(proc.stderr).text(),
            proc.exited
        ])
        return { stdout, stderr, exitCode }
    } finally {
        clearTimeout(timeout)
    }
}

/**
 * @param {string} label
 * @param {Awaited<ReturnType<typeof run>>} result
 */
function expectSuccess(label, result) {
    expect(result.exitCode, `${label} stderr:\n${result.stderr}\nstdout:\n${result.stdout}`).toBe(0)
}

/**
 * @param {string} prefix
 */
async function tempRoot(prefix) {
    const root = await mkdtemp(path.join(os.tmpdir(), prefix))
    roots.push(root)
    return root
}

/**
 * @param {string} prefix
 */
async function tempWorkspace(prefix) {
    const root = await mkdtemp(path.join(os.tmpdir(), prefix))
    workspaces.push(root)
    return root
}

/**
 * @param {string} command
 */
async function requireCommand(command) {
    const result = await run(['bash', '-lc', `command -v ${command}`])
    expectSuccess(`required command ${command}`, result)
}

beforeAll(async () => {
    await mkdir(path.dirname(binaryPath), { recursive: true })
    const build = await run(['bun', 'run', 'build:exe'], { timeout: 120_000 })
    expectSuccess('bun run build:exe', build)
})

afterAll(async () => {
    await Promise.all([
        ...roots.map((root) => rm(root, { recursive: true, force: true })),
        ...workspaces.map((root) => rm(root, { recursive: true, force: true }))
    ])
})

describe('compiled binary language interop', () => {
    test('Python can drive FYLO through the machine JSON protocol', async () => {
        await requireCommand('python3')
        const root = await tempRoot('fylo-python-')
        const workspace = await tempWorkspace('fylo-python-src-')
        const script = path.join(workspace, 'interop.py')
        await writeFile(
            script,
            String.raw`import json
import subprocess
import sys

binary, root = sys.argv[1], sys.argv[2]
collection = "interop-python"

def call(request):
    completed = subprocess.run(
        [binary, "exec", "--request", json.dumps(request)],
        check=False,
        text=True,
        capture_output=True,
    )
    if completed.returncode != 0:
        raise RuntimeError(completed.stderr + completed.stdout)
    payload = json.loads(completed.stdout)
    if not payload.get("ok"):
        raise RuntimeError(completed.stdout)
    return payload["result"]

call({"op": "createCollection", "root": root, "collection": collection})
doc_id = call({
    "op": "putData",
    "root": root,
    "collection": collection,
    "data": {"language": "python", "active": True, "score": 41},
})
latest = call({"op": "getLatest", "root": root, "collection": collection, "id": doc_id})
assert latest[doc_id]["language"] == "python"
found = call({
    "op": "findDocs",
    "root": root,
    "collection": collection,
    "query": {"$ops": [{"score": {"$gte": 40}}]},
})
assert found[doc_id]["active"] is True
print(json.dumps({"ok": True, "id": doc_id}))
`
        )

        const result = await run(['python3', script, binaryPath, root])
        expectSuccess('python interop', result)
        expect(JSON.parse(result.stdout).ok).toBe(true)
    })

    test('Ruby can drive FYLO through the machine JSON protocol', async () => {
        await requireCommand('ruby')
        const root = await tempRoot('fylo-ruby-')
        const workspace = await tempWorkspace('fylo-ruby-src-')
        const script = path.join(workspace, 'interop.rb')
        await writeFile(
            script,
            String.raw`require "json"
require "open3"

binary, root = ARGV
collection = "interop-ruby"

def call(binary, request)
  stdout, stderr, status = Open3.capture3(binary, "exec", "--request", JSON.generate(request))
  raise stderr + stdout unless status.success?
  payload = JSON.parse(stdout)
  raise stdout unless payload["ok"]
  payload["result"]
end

call(binary, {"op" => "createCollection", "root" => root, "collection" => collection})
doc_id = call(binary, {
  "op" => "putData",
  "root" => root,
  "collection" => collection,
  "data" => {"language" => "ruby", "active" => true, "score" => 42}
})
latest = call(binary, {"op" => "getLatest", "root" => root, "collection" => collection, "id" => doc_id})
raise "getLatest did not round-trip Ruby document" unless latest[doc_id]["language"] == "ruby"
found = call(binary, {
  "op" => "findDocs",
  "root" => root,
  "collection" => collection,
  "query" => {"$ops" => [{"score" => {"$gte" => 40}}]}
})
raise "findDocs did not return Ruby document" unless found[doc_id]["active"] == true
puts JSON.generate({"ok" => true, "id" => doc_id})
`
        )

        const result = await run(['ruby', script, binaryPath, root])
        expectSuccess('ruby interop', result)
        expect(JSON.parse(result.stdout).ok).toBe(true)
    })

    test('PHP can drive FYLO through the machine JSON protocol', async () => {
        await requireCommand('php')
        const root = await tempRoot('fylo-php-')
        const workspace = await tempWorkspace('fylo-php-src-')
        const script = path.join(workspace, 'interop.php')
        await writeFile(
            script,
            String.raw`<?php
$binary = $argv[1];
$root = $argv[2];
$collection = "interop-php";

function call_fylo(string $binary, array $request): mixed {
    $pipes = [];
    $process = proc_open(
        [$binary, "exec", "--request", json_encode($request, JSON_THROW_ON_ERROR)],
        [1 => ["pipe", "w"], 2 => ["pipe", "w"]],
        $pipes
    );
    if (!is_resource($process)) {
        throw new RuntimeException("Unable to start FYLO process");
    }
    $stdout = stream_get_contents($pipes[1]);
    $stderr = stream_get_contents($pipes[2]);
    fclose($pipes[1]);
    fclose($pipes[2]);
    $exit = proc_close($process);
    if ($exit !== 0) {
        throw new RuntimeException($stderr . $stdout);
    }
    $payload = json_decode($stdout, true, flags: JSON_THROW_ON_ERROR);
    if (($payload["ok"] ?? false) !== true) {
        throw new RuntimeException($stdout);
    }
    return $payload["result"];
}

call_fylo($binary, ["op" => "createCollection", "root" => $root, "collection" => $collection]);
$docId = call_fylo($binary, [
    "op" => "putData",
    "root" => $root,
    "collection" => $collection,
    "data" => ["language" => "php", "active" => true, "score" => 43],
]);
$latest = call_fylo($binary, ["op" => "getLatest", "root" => $root, "collection" => $collection, "id" => $docId]);
if ($latest[$docId]["language"] !== "php") {
    throw new RuntimeException("getLatest did not round-trip PHP document");
}
$found = call_fylo($binary, [
    "op" => "findDocs",
    "root" => $root,
    "collection" => $collection,
    "query" => ['$ops' => [["score" => ['$gte' => 40]]]],
]);
if ($found[$docId]["active"] !== true) {
    throw new RuntimeException("findDocs did not return PHP document");
}
echo json_encode(["ok" => true, "id" => $docId], JSON_THROW_ON_ERROR) . PHP_EOL;
`
        )

        const result = await run(['php', script, binaryPath, root])
        expectSuccess('php interop', result)
        expect(JSON.parse(result.stdout).ok).toBe(true)
    })

    test('Dart can drive FYLO through the machine JSON protocol', async () => {
        await requireCommand('dart')
        const root = await tempRoot('fylo-dart-')
        const workspace = await tempWorkspace('fylo-dart-src-')
        const script = path.join(workspace, 'interop.dart')
        await writeFile(
            script,
            String.raw`import 'dart:convert';
import 'dart:io';

Future<dynamic> callFylo(String binary, Map<String, dynamic> request) async {
  final result = await Process.run(binary, ['exec', '--request', jsonEncode(request)]);
  if (result.exitCode != 0) {
    throw StateError('\${result.stderr}\${result.stdout}');
  }
  final payload = jsonDecode(result.stdout as String) as Map<String, dynamic>;
  if (payload['ok'] != true) {
    throw StateError(result.stdout as String);
  }
  return payload['result'];
}

Future<void> main(List<String> args) async {
  final binary = args[0];
  final root = args[1];
  const collection = 'interop-dart';
  await callFylo(binary, {'op': 'createCollection', 'root': root, 'collection': collection});
  final docId = await callFylo(binary, {
    'op': 'putData',
    'root': root,
    'collection': collection,
    'data': {'language': 'dart', 'active': true, 'score': 44},
  }) as String;
  final latest = await callFylo(binary, {'op': 'getLatest', 'root': root, 'collection': collection, 'id': docId}) as Map<String, dynamic>;
  if ((latest[docId] as Map<String, dynamic>)['language'] != 'dart') {
    throw StateError('getLatest did not round-trip Dart document');
  }
  final found = await callFylo(binary, {
    'op': 'findDocs',
    'root': root,
    'collection': collection,
    'query': {r'$ops': [{'score': {r'$gte': 40}}]},
  }) as Map<String, dynamic>;
  if ((found[docId] as Map<String, dynamic>)['active'] != true) {
    throw StateError('findDocs did not return Dart document');
  }
  print(jsonEncode({'ok': true, 'id': docId}));
}
`
        )

        const result = await run(['dart', script, binaryPath, root], { timeout: 120_000 })
        expectSuccess('dart interop', result)
        expect(JSON.parse(result.stdout).ok).toBe(true)
    })

    test('Java can drive FYLO through the machine JSON protocol', async () => {
        await requireCommand('javac')
        await requireCommand('java')
        const root = await tempRoot('fylo-java-')
        const workspace = await tempWorkspace('fylo-java-src-')
        const source = path.join(workspace, 'Interop.java')
        await writeFile(
            source,
            String.raw`import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.util.List;

public class Interop {
    static String escapeJson(String value) {
        return value.replace("\\", "\\\\").replace("\"", "\\\"");
    }

    static String extractResultString(String payload) {
        String marker = "\"result\":\"";
        int start = payload.indexOf(marker);
        if (start < 0) throw new IllegalStateException("missing result string: " + payload);
        start += marker.length();
        int end = payload.indexOf("\"", start);
        if (end < 0) throw new IllegalStateException("unterminated result string: " + payload);
        return payload.substring(start, end);
    }

    static String call(String binary, String request) throws IOException, InterruptedException {
        Process process = new ProcessBuilder(List.of(binary, "exec", "--request", request)).start();
        String stdout = new String(process.getInputStream().readAllBytes(), StandardCharsets.UTF_8);
        String stderr = new String(process.getErrorStream().readAllBytes(), StandardCharsets.UTF_8);
        int exit = process.waitFor();
        if (exit != 0) throw new IllegalStateException(stderr + stdout);
        if (!stdout.contains("\"ok\":true")) throw new IllegalStateException(stdout);
        return stdout;
    }

    public static void main(String[] args) throws Exception {
        String binary = args[0];
        String root = escapeJson(args[1]);
        String collection = "interop-java";
        call(binary, String.format("{\"op\":\"createCollection\",\"root\":\"%s\",\"collection\":\"%s\"}", root, collection));
        String put = call(binary, String.format(
            "{\"op\":\"putData\",\"root\":\"%s\",\"collection\":\"%s\",\"data\":{\"language\":\"java\",\"active\":true,\"score\":45}}",
            root,
            collection
        ));
        String id = extractResultString(put);
        String latest = call(binary, String.format(
            "{\"op\":\"getLatest\",\"root\":\"%s\",\"collection\":\"%s\",\"id\":\"%s\"}",
            root,
            collection,
            id
        ));
        if (!latest.contains("\"language\":\"java\"")) {
            throw new IllegalStateException("getLatest did not round-trip Java document: " + latest);
        }
        String found = call(binary, String.format(
            "{\"op\":\"findDocs\",\"root\":\"%s\",\"collection\":\"%s\",\"query\":{\"$ops\":[{\"score\":{\"$gte\":40}}]}}",
            root,
            collection
        ));
        if (!found.contains("\"" + id + "\":") || !found.contains("\"active\":true")) {
            throw new IllegalStateException("findDocs did not return Java document: " + found);
        }
        System.out.println("{\"ok\":true,\"id\":\"" + id + "\"}");
    }
}
`
        )
        const compile = await run(['javac', source], { timeout: 120_000 })
        expectSuccess('javac interop compile', compile)

        const result = await run(['java', '-cp', workspace, 'Interop', binaryPath, root], {
            timeout: 120_000
        })
        expectSuccess('java interop', result)
        expect(JSON.parse(result.stdout).ok).toBe(true)
    })

    test('C# can drive FYLO through the machine JSON protocol', async () => {
        await requireCommand('dotnet')
        const root = await tempRoot('fylo-csharp-')
        const workspace = await tempWorkspace('fylo-csharp-src-')
        await writeFile(
            path.join(workspace, 'Interop.csproj'),
            String.raw`<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <OutputType>Exe</OutputType>
    <TargetFramework>net8.0</TargetFramework>
    <ImplicitUsings>enable</ImplicitUsings>
    <Nullable>enable</Nullable>
  </PropertyGroup>
</Project>
`
        )
        await writeFile(
            path.join(workspace, 'Program.cs'),
            String.raw`using System.Diagnostics;
using System.Text.Json;

static JsonElement CallFylo(string binary, object request)
{
    var body = JsonSerializer.Serialize(request);
    using var process = new Process();
    process.StartInfo.FileName = binary;
    process.StartInfo.ArgumentList.Add("exec");
    process.StartInfo.ArgumentList.Add("--request");
    process.StartInfo.ArgumentList.Add(body);
    process.StartInfo.RedirectStandardOutput = true;
    process.StartInfo.RedirectStandardError = true;
    process.Start();
    var stdout = process.StandardOutput.ReadToEnd();
    var stderr = process.StandardError.ReadToEnd();
    process.WaitForExit();
    if (process.ExitCode != 0) throw new Exception(stderr + stdout);
    using var document = JsonDocument.Parse(stdout);
    if (!document.RootElement.GetProperty("ok").GetBoolean()) throw new Exception(stdout);
    return document.RootElement.GetProperty("result").Clone();
}

var binary = args[0];
var root = args[1];
const string collection = "interop-csharp";
CallFylo(binary, new { op = "createCollection", root, collection });
var id = CallFylo(binary, new {
    op = "putData",
    root,
    collection,
    data = new { language = "csharp", active = true, score = 46 }
}).GetString()!;
var latest = CallFylo(binary, new { op = "getLatest", root, collection, id });
if (latest.GetProperty(id).GetProperty("language").GetString() != "csharp")
    throw new Exception("getLatest did not round-trip C# document");
var found = CallFylo(binary, new {
    op = "findDocs",
    root,
    collection,
    query = new Dictionary<string, object> {
        ["$ops"] = new object[] {
            new Dictionary<string, object> {
                ["score"] = new Dictionary<string, object> { ["$gte"] = 40 }
            }
        }
    }
});
if (!found.GetProperty(id).GetProperty("active").GetBoolean())
    throw new Exception("findDocs did not return C# document");
Console.WriteLine(JsonSerializer.Serialize(new { ok = true, id }));
`
        )

        const result = await run(
            ['dotnet', 'run', '--project', workspace, '--', binaryPath, root],
            {
                timeout: 120_000
            }
        )
        expectSuccess('csharp interop', result)
        expect(JSON.parse(result.stdout).ok).toBe(true)
    })

    test('C++ can drive FYLO through the machine JSON protocol', async () => {
        await requireCommand('c++')
        const root = await tempRoot('fylo-cpp-')
        const workspace = await tempWorkspace('fylo-cpp-src-')
        const source = path.join(workspace, 'interop.cpp')
        const executable = path.join(workspace, 'interop-cpp')
        await writeFile(
            source,
            String.raw`#include <array>
#include <cstdio>
#include <cstdlib>
#include <iostream>
#include <stdexcept>
#include <string>

std::string shell_quote(const std::string& value) {
    std::string out = "'";
    for (char ch : value) {
        if (ch == '\'') out += "'\\''";
        else out += ch;
    }
    out += "'";
    return out;
}

std::string json_escape(std::string value) {
    std::string out;
    for (char ch : value) {
        if (ch == '\\') out += "\\\\";
        else if (ch == '"') out += "\\\"";
        else out += ch;
    }
    return out;
}

std::string extract_result_string(const std::string& payload) {
    const std::string marker = "\"result\":\"";
    auto start = payload.find(marker);
    if (start == std::string::npos) throw std::runtime_error("missing result string: " + payload);
    start += marker.size();
    auto end = payload.find('"', start);
    if (end == std::string::npos) throw std::runtime_error("unterminated result string: " + payload);
    return payload.substr(start, end - start);
}

std::string call(const std::string& binary, const std::string& request) {
    std::string command = shell_quote(binary) + " exec --request " + shell_quote(request) + " 2>&1";
    std::array<char, 4096> buffer{};
    std::string output;
    FILE* pipe = popen(command.c_str(), "r");
    if (!pipe) throw std::runtime_error("failed to start FYLO binary");
    while (fgets(buffer.data(), buffer.size(), pipe) != nullptr) output += buffer.data();
    int status = pclose(pipe);
    if (status != 0) throw std::runtime_error(output);
    if (output.find("\"ok\":true") == std::string::npos) throw std::runtime_error(output);
    return output;
}

int main(int argc, char** argv) {
    std::string binary = argv[1];
    std::string root = json_escape(argv[2]);
    std::string collection = "interop-cpp";
    call(binary, "{\"op\":\"createCollection\",\"root\":\"" + root + "\",\"collection\":\"" + collection + "\"}");
    std::string put = call(binary, "{\"op\":\"putData\",\"root\":\"" + root + "\",\"collection\":\"" + collection + "\",\"data\":{\"language\":\"cpp\",\"active\":true,\"score\":47}}");
    std::string id = extract_result_string(put);
    std::string latest = call(binary, "{\"op\":\"getLatest\",\"root\":\"" + root + "\",\"collection\":\"" + collection + "\",\"id\":\"" + id + "\"}");
    if (latest.find("\"language\":\"cpp\"") == std::string::npos) throw std::runtime_error("getLatest did not round-trip C++ document");
    std::string found = call(binary, "{\"op\":\"findDocs\",\"root\":\"" + root + "\",\"collection\":\"" + collection + "\",\"query\":{\"$ops\":[{\"score\":{\"$gte\":40}}]}}");
    if (found.find("\"" + id + "\":") == std::string::npos || found.find("\"active\":true") == std::string::npos)
        throw std::runtime_error("findDocs did not return C++ document");
    std::cout << "{\"ok\":true,\"id\":\"" << id << "\"}" << std::endl;
}
`
        )
        const compile = await run(['c++', '-std=c++17', source, '-o', executable], {
            timeout: 120_000
        })
        expectSuccess('c++ interop compile', compile)

        const result = await run([executable, binaryPath, root], { timeout: 120_000 })
        expectSuccess('c++ interop', result)
        expect(JSON.parse(result.stdout).ok).toBe(true)
    })

    test('Swift can drive FYLO through the machine JSON protocol', async () => {
        await requireCommand('swift')
        const root = await tempRoot('fylo-swift-')
        const workspace = await tempWorkspace('fylo-swift-src-')
        const source = path.join(workspace, 'interop.swift')
        await writeFile(
            source,
            String.raw`import Foundation

func jsonEscape(_ value: String) -> String {
    value.replacingOccurrences(of: "\\", with: "\\\\").replacingOccurrences(of: "\"", with: "\\\"")
}

func extractResultString(_ payload: String) throws -> String {
    let marker = "\"result\":\""
    guard let range = payload.range(of: marker) else { throw NSError(domain: "Interop", code: 1) }
    let tail = payload[range.upperBound...]
    guard let end = tail.firstIndex(of: "\"") else { throw NSError(domain: "Interop", code: 2) }
    return String(tail[..<end])
}

func call(_ binary: String, _ request: String) throws -> String {
    let process = Process()
    process.executableURL = URL(fileURLWithPath: binary)
    process.arguments = ["exec", "--request", request]
    let stdout = Pipe()
    let stderr = Pipe()
    process.standardOutput = stdout
    process.standardError = stderr
    try process.run()
    process.waitUntilExit()
    let out = String(data: stdout.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? ""
    let err = String(data: stderr.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? ""
    if process.terminationStatus != 0 { throw NSError(domain: err + out, code: Int(process.terminationStatus)) }
    if !out.contains("\"ok\":true") { throw NSError(domain: out, code: 3) }
    return out
}

let binary = CommandLine.arguments[1]
let root = jsonEscape(CommandLine.arguments[2])
let collection = "interop-swift"
_ = try call(binary, "{\"op\":\"createCollection\",\"root\":\"\(root)\",\"collection\":\"\(collection)\"}")
let put = try call(binary, "{\"op\":\"putData\",\"root\":\"\(root)\",\"collection\":\"\(collection)\",\"data\":{\"language\":\"swift\",\"active\":true,\"score\":48}}")
let id = try extractResultString(put)
let latest = try call(binary, "{\"op\":\"getLatest\",\"root\":\"\(root)\",\"collection\":\"\(collection)\",\"id\":\"\(id)\"}")
if !latest.contains("\"language\":\"swift\"") { throw NSError(domain: "getLatest did not round-trip Swift document", code: 4) }
let found = try call(binary, "{\"op\":\"findDocs\",\"root\":\"\(root)\",\"collection\":\"\(collection)\",\"query\":{\"$ops\":[{\"score\":{\"$gte\":40}}]}}")
if !found.contains("\"\(id)\":") || !found.contains("\"active\":true") {
    throw NSError(domain: "findDocs did not return Swift document", code: 5)
}
print("{\"ok\":true,\"id\":\"\(id)\"}")
`
        )

        const result = await run(['swift', source, binaryPath, root], { timeout: 120_000 })
        expectSuccess('swift interop', result)
        expect(JSON.parse(result.stdout).ok).toBe(true)
    })

    test('Kotlin can drive FYLO through the machine JSON protocol', async () => {
        await requireCommand('kotlinc')
        await requireCommand('java')
        const root = await tempRoot('fylo-kotlin-')
        const workspace = await tempWorkspace('fylo-kotlin-src-')
        const source = path.join(workspace, 'Interop.kt')
        const jar = path.join(workspace, 'interop.jar')
        await writeFile(
            source,
            String.raw`import java.io.File

fun jsonEscape(value: String): String = value.replace("\\", "\\\\").replace("\"", "\\\"")

fun extractResultString(payload: String): String {
    val marker = "\"result\":\""
    val start = payload.indexOf(marker)
    require(start >= 0) { "missing result string: $payload" }
    val valueStart = start + marker.length
    val end = payload.indexOf("\"", valueStart)
    require(end >= 0) { "unterminated result string: $payload" }
    return payload.substring(valueStart, end)
}

fun call(binary: String, request: String): String {
    val process = ProcessBuilder(binary, "exec", "--request", request).start()
    val stdout = process.inputStream.bufferedReader().readText()
    val stderr = process.errorStream.bufferedReader().readText()
    val exit = process.waitFor()
    require(exit == 0) { stderr + stdout }
    require(stdout.contains("\"ok\":true")) { stdout }
    return stdout
}

fun main(args: Array<String>) {
    val binary = args[0]
    val root = jsonEscape(args[1])
    val collection = "interop-kotlin"
    call(binary, "{\"op\":\"createCollection\",\"root\":\"$root\",\"collection\":\"$collection\"}")
    val put = call(binary, "{\"op\":\"putData\",\"root\":\"$root\",\"collection\":\"$collection\",\"data\":{\"language\":\"kotlin\",\"active\":true,\"score\":49}}")
    val id = extractResultString(put)
    val latest = call(binary, "{\"op\":\"getLatest\",\"root\":\"$root\",\"collection\":\"$collection\",\"id\":\"$id\"}")
    require(latest.contains("\"language\":\"kotlin\"")) { "getLatest did not round-trip Kotlin document: $latest" }
    val found = call(binary, "{\"op\":\"findDocs\",\"root\":\"$root\",\"collection\":\"$collection\",\"query\":{\"$" + "ops\":[{\"score\":{\"$" + "gte\":40}}]}}")
    require(found.contains("\"$id\":") && found.contains("\"active\":true")) { "findDocs did not return Kotlin document: $found" }
    println("{\"ok\":true,\"id\":\"$id\"}")
}
`
        )
        const compile = await run(['kotlinc', source, '-include-runtime', '-d', jar], {
            timeout: 120_000
        })
        expectSuccess('kotlinc interop compile', compile)

        const result = await run(['java', '-jar', jar, binaryPath, root], { timeout: 120_000 })
        expectSuccess('kotlin interop', result)
        expect(JSON.parse(result.stdout).ok).toBe(true)
    })

    test('Rust can drive FYLO through the machine JSON protocol', async () => {
        await requireCommand('rustc')
        const root = await tempRoot('fylo-rust-')
        const workspace = await tempWorkspace('fylo-rust-src-')
        const source = path.join(workspace, 'interop.rs')
        const executable = path.join(workspace, 'interop-rust')
        await writeFile(
            source,
            String.raw`use std::env;
use std::process::Command;

fn escape_json(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn extract_result_string(payload: &str) -> String {
    let marker = "\"result\":\"";
    let start = payload.find(marker).expect("missing result string") + marker.len();
    let tail = &payload[start..];
    let end = tail.find('"').expect("unterminated result string");
    tail[..end].to_string()
}

fn call(binary: &str, request: &str) -> String {
    let output = Command::new(binary)
        .args(["exec", "--request", request])
        .output()
        .expect("failed to run fylo binary");
    if !output.status.success() {
        panic!("{}", String::from_utf8_lossy(&output.stderr));
    }
    let stdout = String::from_utf8(output.stdout).expect("stdout is not utf8");
    if !stdout.contains("\"ok\":true") {
        panic!("{}", stdout);
    }
    stdout
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let binary = &args[1];
    let root = escape_json(&args[2]);
    let collection = "interop-rust";
    call(binary, &format!(r#"{{"op":"createCollection","root":"{}","collection":"{}"}}"#, root, collection));
    let put = call(binary, &format!(
        r#"{{"op":"putData","root":"{}","collection":"{}","data":{{"language":"rust","active":true,"score":50}}}}"#,
        root, collection
    ));
    let id = extract_result_string(&put);
    let latest = call(binary, &format!(
        r#"{{"op":"getLatest","root":"{}","collection":"{}","id":"{}"}}"#,
        root, collection, id
    ));
    if !latest.contains("\"language\":\"rust\"") {
        panic!("getLatest did not round-trip Rust document: {}", latest);
    }
    let found = call(binary, &format!(
        r#"{{"op":"findDocs","root":"{}","collection":"{}","query":{{"$ops":[{{"score":{{"$gte":40}}}}]}}}}"#,
        root, collection
    ));
    if !found.contains(&format!("\"{}\":", id)) || !found.contains("\"active\":true") {
        panic!("findDocs did not return Rust document: {}", found);
    }
    println!("{{\"ok\":true,\"id\":\"{}\"}}", id);
}
`
        )
        const compile = await run(['rustc', source, '-o', executable], { timeout: 120_000 })
        expectSuccess('rustc interop compile', compile)

        const result = await run([executable, binaryPath, root], { timeout: 120_000 })
        expectSuccess('rust interop', result)
        expect(JSON.parse(result.stdout).ok).toBe(true)
    })
})
