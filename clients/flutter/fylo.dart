// FYLO Flutter client — a local-first document store for Flutter apps.
//
// Flutter apps can't spawn the `fylo` binary, so this hosts FYLO's local-first
// web engine (fylo.mjs) in a headless in-app WebView (flutter_inappwebview) and
// bridges Dart <-> JS. Reads/writes hit an on-device OPFS store (fully offline);
// a background SyncEngine reconciles with a backend `fylo serve` over REST/SSE.
// Same engine as the browser/iOS/Android clients, native async API.
//
//   final db = await Fylo.open(serverUrl: 'https://api.example.com', token: token);
//   await db.collection('users').put({'name': 'Ada', 'role': 'admin'});
//   final admins = await db.collection('users').find({r'$ops': [{'role': {r'$eq': 'admin'}}]});
//   await db.syncStart(); // background sync; omit serverUrl to stay offline
//
// Bundle the engine assets in your app's pubspec under assets/fylo/: fylo.mjs
// (from a FYLO release) plus host.html and bridge.js from clients/mobile/. They
// are served over http://localhost by an in-app server — a secure context, so
// OPFS is available (a file:// origin would not get OPFS). On iOS, add an App
// Transport Security exception so the WebView can reach the localhost server:
// Info.plist -> NSAppTransportSecurity -> NSAllowsLocalNetworking = true.
//
// Depends on flutter_inappwebview (^6.0.0). For Flutter *web*, use fylo-web.mjs
// directly via JS interop instead — a WebView isn't needed inside a browser.

import 'dart:async';
import 'dart:convert';

import 'package:flutter_inappwebview/flutter_inappwebview.dart';

class FyloException implements Exception {
  final String message;
  FyloException(this.message);
  @override
  String toString() => 'FyloException: $message';
}

class Fylo {
  final HeadlessInAppWebView _webView;
  final InAppLocalhostServer _server;
  final Map<int, Completer<dynamic>> _pending = {};
  final Completer<void> _ready = Completer<void>();
  int _nextId = 1;

  Fylo._(this._webView, this._server);

  /// Boot the engine. Resolves once the local store is ready; [serverUrl]/[token]
  /// enable backend sync (omit for a pure offline store). [assetPath] is where
  /// the three engine assets live in the app bundle; [port] is the localhost
  /// port the assets are served on (secure context for OPFS).
  static Future<Fylo> open({
    String? serverUrl,
    String? token,
    String assetPath = 'assets/fylo',
    int port = 8459,
  }) async {
    final server = InAppLocalhostServer(documentRoot: assetPath, port: port);
    await server.start();

    late final Fylo client;
    final webView = HeadlessInAppWebView(
      initialUrlRequest: URLRequest(url: WebUri('http://localhost:$port/host.html')),
      onWebViewCreated: (controller) {
        controller.addJavaScriptHandler(
          handlerName: 'fylo',
          callback: (args) {
            if (args.isNotEmpty) client._onMessage(args.first as String);
            return null;
          },
        );
      },
    );

    client = Fylo._(webView, server);
    await webView.run();
    await client._ready.future; // bridge posts id 0 when the engine is ready

    final config = <String, dynamic>{};
    if (serverUrl != null) config['serverUrl'] = serverUrl;
    if (token != null) config['token'] = token;
    await client.request('open', {'config': config});
    return client;
  }

  void _onMessage(String text) {
    final reply = jsonDecode(text) as Map<String, dynamic>;
    final id = reply['id'] as int? ?? -1;
    if (id == 0) {
      if (!_ready.isCompleted) _ready.complete();
      return;
    }
    final completer = _pending.remove(id);
    if (completer == null) return;
    if (reply['ok'] == true) {
      completer.complete(reply['result']);
    } else {
      final error = reply['error'];
      completer.completeError(
        FyloException((error is Map ? error['message'] : null) ?? 'fylo error'),
      );
    }
  }

  /// Send one RPC to the engine and await its result.
  Future<dynamic> request(String method, [Map<String, dynamic> args = const {}]) {
    final id = _nextId++;
    final payload = jsonEncode({'id': id, 'method': method, 'args': args});
    final encoded = base64Encode(utf8.encode(payload));
    final completer = Completer<dynamic>();
    _pending[id] = completer;
    // base64 body is safe inside the JS string literal.
    _webView.webViewController?.evaluateJavascript(
      source: 'window.__fyloDispatch("$encoded")',
    );
    return completer.future;
  }

  // --- Collections ---
  Future<dynamic> createCollection(String collection, [String kind = 'document']) =>
      request('createCollection', {'collection': collection, 'kind': kind});
  Future<dynamic> dropCollection(String collection) =>
      request('dropCollection', {'collection': collection});
  Future<dynamic> inspectCollection(String collection) =>
      request('inspectCollection', {'collection': collection});
  Future<dynamic> rebuildCollection(String collection) =>
      request('rebuildCollection', {'collection': collection});

  // --- Documents ---
  Future<dynamic> putData(String collection, Map<String, dynamic> data) =>
      request('putData', {'collection': collection, 'data': data});
  Future<dynamic> getDoc(String collection, String id) =>
      request('getDoc', {'collection': collection, 'id': id});
  Future<dynamic> getLatest(String collection, String id) =>
      request('getLatest', {'collection': collection, 'id': id});
  Future<dynamic> patchDoc(String collection, String id, Map<String, dynamic> newDoc) =>
      request('patchDoc', {'collection': collection, 'id': id, 'newDoc': newDoc});
  Future<dynamic> delDoc(String collection, String id) =>
      request('delDoc', {'collection': collection, 'id': id});
  Future<dynamic> restoreDoc(String collection, String id) =>
      request('restoreDoc', {'collection': collection, 'id': id});

  // --- Query ---
  Future<dynamic> findDocs(String collection, Map<String, dynamic> query) =>
      request('findDocs', {'collection': collection, 'query': query});

  /// Run raw SQL against the local store. Native interpolation is verbatim —
  /// escape/validate untrusted input yourself. SQL writes are local-only (not
  /// pushed); use the document methods to sync writes.
  Future<dynamic> sql(String statement) => request('executeSQL', {'sql': statement});

  // --- Sync ---
  Future<void> syncStart() async {
    await request('syncStart');
  }

  Future<void> syncStop() async {
    await request('syncStop');
  }

  Future<bool> isOnline() async {
    final result = await request('online');
    return result is Map && result['online'] == true;
  }

  /// Collection-scoped facade with short method names, so
  /// `db.collection('users').put(data)` reads like the browser client.
  FyloCollection collection(String name) => FyloCollection(this, name);

  /// Dispose the WebView and stop the localhost asset server.
  Future<void> close() async {
    await _webView.dispose();
    await _server.close();
  }
}

/// A collection-scoped view; methods drop the leading collection argument.
class FyloCollection {
  final Fylo _db;
  final String _name;

  FyloCollection(this._db, this._name);

  Future<dynamic> create([String kind = 'document']) => _db.createCollection(_name, kind);
  Future<dynamic> drop() => _db.dropCollection(_name);
  Future<dynamic> inspect() => _db.inspectCollection(_name);
  Future<dynamic> rebuild() => _db.rebuildCollection(_name);
  Future<dynamic> put(Map<String, dynamic> data) => _db.putData(_name, data);
  Future<dynamic> get(String id) => _db.getDoc(_name, id);
  Future<dynamic> latest(String id) => _db.getLatest(_name, id);
  Future<dynamic> patch(String id, Map<String, dynamic> newDoc) =>
      _db.patchDoc(_name, id, newDoc);
  Future<dynamic> delete(String id) => _db.delDoc(_name, id);
  Future<dynamic> restore(String id) => _db.restoreDoc(_name, id);
  Future<dynamic> find(Map<String, dynamic> query) => _db.findDocs(_name, query);
}
