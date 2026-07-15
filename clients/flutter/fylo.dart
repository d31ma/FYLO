// FYLO Flutter client — a local-first document store for Flutter apps.
//
// Flutter apps can't spawn the `fylo` binary, so this hosts FYLO's local-first
// web engine (fylo.mjs) in a headless in-app WebView (flutter_inappwebview) and
// bridges Dart <-> JS. All reads and writes hit an on-device OPFS store —
// fully offline, no backend. Same engine as the browser/iOS/Android clients,
// native async API.
//
//   final db = await Fylo.open();
//   await db.collection('users').put({'name': 'Ada', 'role': 'admin'});
//   final admins = await db.collection('users').find({r'$ops': [{'role': {r'$eq': 'admin'}}]});
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
import 'dart:typed_data';

import 'package:flutter_inappwebview/flutter_inappwebview.dart';

class FyloException implements Exception {
  final String message;
  FyloException(this.message);
  @override
  String toString() => 'FyloException: $message';
}

class _PendingRequest {
  final Completer<dynamic> completer;
  final Timer timeout;

  _PendingRequest(this.completer, this.timeout);

  void complete(dynamic value) {
    timeout.cancel();
    if (!completer.isCompleted) completer.complete(value);
  }

  void fail(Object error, [StackTrace? stackTrace]) {
    timeout.cancel();
    if (!completer.isCompleted) completer.completeError(error, stackTrace);
  }
}

class Fylo {
  static const int _maxBridgeRequestBytes = 6 * 1024 * 1024;
  static const int _maxBridgeResponseBytes = 6 * 1024 * 1024;
  static const int _maxPendingRequests = 256;
  static const Duration _defaultRpcTimeout = Duration(seconds: 30);
  static const Duration _maxRpcTimeout = Duration(minutes: 5);
  static const Set<String> _allowedAssetPaths = {
    '/host.html',
    '/bridge.js',
    '/fylo.mjs',
  };
  final HeadlessInAppWebView _webView;
  final InAppLocalhostServer _server;
  final Duration _rpcTimeout;
  final Map<int, _PendingRequest> _pending = {};
  final Completer<void> _ready = Completer<void>();
  Future<void>? _disposeFuture;
  bool _closed = false;
  int _nextId = 1;

  Fylo._(this._webView, this._server, this._rpcTimeout);

  /// Boot the engine. Resolves once the local store is ready. [assetPath] is
  /// where the three engine assets live in the app bundle; [port] is the
  /// localhost port the assets are served on (secure context for OPFS).
  static Future<Fylo> open({
    String assetPath = 'assets/fylo',
    int port = 8459,
    Duration rpcTimeout = _defaultRpcTimeout,
  }) async {
    if (rpcTimeout <= Duration.zero || rpcTimeout > _maxRpcTimeout) {
      throw ArgumentError.value(
        rpcTimeout,
        'rpcTimeout',
        'must be greater than zero and at most five minutes',
      );
    }
    final server = InAppLocalhostServer(documentRoot: assetPath, port: port);
    await server.start();

    late final Fylo client;
    final origin = 'http://localhost:$port';
    final webView = HeadlessInAppWebView(
      initialUrlRequest: URLRequest(url: WebUri('$origin/host.html')),
      initialSettings: InAppWebViewSettings(
        useShouldOverrideUrlLoading: true,
      ),
      onWebViewCreated: (controller) {
        controller.addJavaScriptHandler(
          handlerName: 'fylo',
          callback: (args) async {
            final current =
                Uri.tryParse((await controller.getUrl()).toString());
            if (!_isAllowedUrl(current, port, documentOnly: true)) return null;
            if (args.isNotEmpty) client._onMessage(args.first as String);
            return null;
          },
        );
      },
      shouldOverrideUrlLoading: (controller, navigationAction) async {
        final uri =
            Uri.tryParse(navigationAction.request.url?.toString() ?? '');
        final allowed = _isAllowedUrl(uri, port, documentOnly: true);
        if (!allowed) {
          client._terminate(
            FyloException('FYLO WebView blocked an unexpected navigation'),
          );
          return NavigationActionPolicy.CANCEL;
        }
        if (client._ready.isCompleted) {
          client._terminate(
            FyloException(
              'FYLO WebView unexpectedly navigated after becoming ready',
            ),
          );
          return NavigationActionPolicy.CANCEL;
        }
        return NavigationActionPolicy.ALLOW;
      },
      shouldInterceptRequest: (controller, request) async {
        final uri = Uri.tryParse(request.url.toString());
        if (request.method == 'GET' &&
            _isAllowedUrl(uri, port) &&
            (uri?.path != '/host.html' || request.isForMainFrame == true)) {
          return null;
        }
        return WebResourceResponse(
          contentType: 'text/plain',
          contentEncoding: 'utf-8',
          data: Uint8List.fromList(utf8.encode('Forbidden')),
          statusCode: 403,
          reasonPhrase: 'Forbidden',
          headers: const {'Cache-Control': 'no-store'},
        );
      },
      onLoadStart: (controller, url) {
        if (client._ready.isCompleted && !client._closed) {
          client._terminate(
            FyloException(
              'FYLO WebView unexpectedly reloaded after becoming ready',
            ),
          );
        }
      },
      onLoadError: (controller, url, code, message) {
        final uri = Uri.tryParse(url?.toString() ?? '');
        if (_isAllowedUrl(uri, port)) {
          client._terminate(
            FyloException('FYLO WebView failed to load ($code): $message'),
          );
        }
      },
      onLoadHttpError: (controller, url, statusCode, description) {
        final uri = Uri.tryParse(url?.toString() ?? '');
        if (_isAllowedUrl(uri, port)) {
          client._terminate(
            FyloException(
              'FYLO WebView load failed with HTTP $statusCode: $description',
            ),
          );
        }
      },
      androidOnRenderProcessGone: (controller, detail) {
        client._terminate(
          FyloException(
            'FYLO WebView render process terminated; reopen the client',
          ),
        );
      },
      iosOnWebContentProcessDidTerminate: (controller) {
        client._terminate(
          FyloException(
            'FYLO WebView content process terminated; reopen the client',
          ),
        );
      },
    );

    client = Fylo._(webView, server, rpcTimeout);
    try {
      final readiness = () async {
        await webView.run();
        await client._ready.future;
      }();
      await readiness.timeout(
        rpcTimeout,
        onTimeout: () {
          final error = FyloException(
            'FYLO WebView did not become ready within ${rpcTimeout.inMilliseconds}ms',
          );
          client._terminate(error);
          throw error;
        },
      );

      await client.request('open', {'config': <String, dynamic>{}});
      return client;
    } catch (_) {
      await client._disposeResources();
      rethrow;
    }
  }

  void _onMessage(String text) {
    final recoveredId = _recoverReplyId(text);
    if (utf8.encode(text).length > _maxBridgeResponseBytes) {
      if (recoveredId != null) {
        _failCorrelated(
          recoveredId,
          'FYLO bridge response exceeds the 6 MiB limit',
        );
      }
      return;
    }
    late final Map<String, dynamic> reply;
    try {
      reply = jsonDecode(text) as Map<String, dynamic>;
    } catch (_) {
      if (recoveredId != null) {
        _failCorrelated(recoveredId, 'invalid FYLO bridge response');
      }
      return;
    }
    final id = reply['id'] as int? ?? -1;
    if (id == 0) {
      if (!_closed && !_ready.isCompleted) _ready.complete();
      return;
    }
    final request = _pending.remove(id);
    if (request == null) return;
    if (reply['ok'] == true) {
      request.complete(reply['result']);
    } else {
      final error = reply['error'];
      request.fail(
        FyloException((error is Map ? error['message'] : null) ?? 'fylo error'),
      );
    }
  }

  void _failCorrelated(int id, String message) {
    final error = FyloException(message);
    if (id == 0) {
      if (!_ready.isCompleted) _ready.completeError(error);
      return;
    }
    _pending.remove(id)?.fail(error);
  }

  static int? _recoverReplyId(String text) {
    final prefix = text.substring(0, text.length < 1024 ? text.length : 1024);
    final match = RegExp(r'^\s*\{\s*"id"\s*:\s*([0-9]{1,15})(?=\s*[,}])')
        .firstMatch(prefix);
    return int.tryParse(match?.group(1) ?? '');
  }

  static bool _isAllowedUrl(Uri? uri, int port, {bool documentOnly = false}) {
    if (uri == null ||
        uri.scheme != 'http' ||
        uri.host != 'localhost' ||
        !uri.hasPort ||
        uri.port != port ||
        uri.userInfo.isNotEmpty ||
        uri.hasQuery ||
        uri.hasFragment ||
        !_allowedAssetPaths.contains(uri.path)) {
      return false;
    }
    return !documentOnly || uri.path == '/host.html';
  }

  /// Send one RPC to the engine and await its result.
  Future<dynamic> request(String method,
      [Map<String, dynamic> args = const {}]) async {
    if (_closed) {
      throw FyloException('FYLO client is closed; reopen it before retrying');
    }
    if (_pending.length >= _maxPendingRequests) {
      throw FyloException('too many pending FYLO requests');
    }
    final id = _nextId++;
    final payload = jsonEncode({'id': id, 'method': method, 'args': args});
    final payloadBytes = utf8.encode(payload);
    if (payloadBytes.length > _maxBridgeRequestBytes) {
      throw FyloException('FYLO request exceeds the 6 MiB native bridge limit');
    }
    final controller = _webView.webViewController;
    if (controller == null) throw FyloException('FYLO WebView is not ready');
    final encoded = base64Encode(payloadBytes);
    final completer = Completer<dynamic>();
    late final _PendingRequest request;
    final timeout = Timer(_rpcTimeout, () {
      if (identical(_pending[id], request)) {
        _pending.remove(id);
        request.fail(
          FyloException(
            "FYLO request '$method' timed out after ${_rpcTimeout.inMilliseconds}ms; verify the WebView is responsive",
          ),
        );
      }
    });
    request = _PendingRequest(completer, timeout);
    _pending[id] = request;
    try {
      // base64 body is safe inside the JS string literal.
      await controller.evaluateJavascript(
          source: 'window.__fyloDispatch("$encoded")');
    } catch (error, stackTrace) {
      _pending.remove(id)?.fail(error, stackTrace);
    }
    return await completer.future;
  }

  // --- Collections ---
  Future<dynamic> createCollection(String collection,
          [String kind = 'document']) =>
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
  Future<dynamic> getMeta(String collection, String id) =>
      request('getMeta', {'collection': collection, 'id': id});
  Future<dynamic> setMeta(
          String collection, String id, Map<String, dynamic> meta) =>
      request('setMeta', {'collection': collection, 'id': id, 'meta': meta});
  Future<dynamic> getLatest(String collection, String id) =>
      request('getLatest', {'collection': collection, 'id': id});
  Future<dynamic> patchDoc(
          String collection, String id, Map<String, dynamic> newDoc) =>
      request(
          'patchDoc', {'collection': collection, 'id': id, 'newDoc': newDoc});
  Future<dynamic> delDoc(String collection, String id) =>
      request('delDoc', {'collection': collection, 'id': id});
  Future<dynamic> restoreDoc(String collection, String id) =>
      request('restoreDoc', {'collection': collection, 'id': id});

  // --- Query ---
  Future<dynamic> findDocs(String collection, Map<String, dynamic> query) =>
      request('findDocs', {'collection': collection, 'query': query});

  /// Run raw SQL against the local store. Native interpolation is verbatim —
  /// escape/validate untrusted input yourself.
  Future<dynamic> sql(String statement) =>
      request('executeSQL', {'sql': statement});

  /// Collection-scoped facade with short method names, so
  /// `db.collection('users').put(data)` reads like the browser client.
  FyloCollection collection(String name) => FyloCollection(this, name);

  /// Dispose the WebView and stop the localhost asset server.
  Future<void> close() async {
    _failBridge(
      FyloException('FYLO client is closed; reopen it before retrying'),
    );
    await _disposeResources();
  }

  void _terminate(FyloException error) {
    _failBridge(error);
    unawaited(_disposeResources());
  }

  void _failBridge(FyloException error) {
    if (_closed) return;
    _closed = true;
    if (!_ready.isCompleted) _ready.completeError(error);
    final requests = _pending.values.toList();
    _pending.clear();
    for (final request in requests) {
      request.fail(error);
    }
  }

  Future<void> _disposeResources() {
    final existing = _disposeFuture;
    if (existing != null) return existing;
    final disposing = _disposeResourcesOnce();
    _disposeFuture = disposing;
    return disposing;
  }

  Future<void> _disposeResourcesOnce() async {
    await _webView.dispose();
    await _server.close();
  }
}

/// A collection-scoped view; methods drop the leading collection argument.
class FyloCollection {
  final Fylo _db;
  final String _name;

  FyloCollection(this._db, this._name);

  Future<dynamic> create([String kind = 'document']) =>
      _db.createCollection(_name, kind);
  Future<dynamic> drop() => _db.dropCollection(_name);
  Future<dynamic> inspect() => _db.inspectCollection(_name);
  Future<dynamic> rebuild() => _db.rebuildCollection(_name);
  Future<dynamic> put(Map<String, dynamic> data) => _db.putData(_name, data);
  Future<dynamic> get(String id) => _db.getDoc(_name, id);
  Future<dynamic> getMeta(String id) => _db.getMeta(_name, id);
  Future<dynamic> setMeta(String id, Map<String, dynamic> meta) =>
      _db.setMeta(_name, id, meta);
  Future<dynamic> latest(String id) => _db.getLatest(_name, id);
  Future<dynamic> patch(String id, Map<String, dynamic> newDoc) =>
      _db.patchDoc(_name, id, newDoc);
  Future<dynamic> delete(String id) => _db.delDoc(_name, id);
  Future<dynamic> restore(String id) => _db.restoreDoc(_name, id);
  Future<dynamic> find(Map<String, dynamic> query) =>
      _db.findDocs(_name, query);
}
