import 'dart:convert';
import 'dart:io';

class ControlApi {
  final Uri endpoint;
  final String token;
  final HttpClient _client = HttpClient();
  ControlApi(this.endpoint, this.token) {
    _client.findProxy = (_) => 'DIRECT';
    _client.connectionTimeout = const Duration(seconds: 5);
  }

  static Future<ControlApi> load({String? descriptorPath}) async {
    final path = descriptorPath ?? Platform.environment['DC_DESCRIPTOR'];
    if (path == null) {
      throw StateError('请通过状态栏菜单打开管理界面');
    }
    final data = jsonDecode(await File(path).readAsString()) as Map<String, dynamic>;
    final endpoint = Uri.parse(data['endpoint'] as String);
    if (endpoint.scheme != 'http' || endpoint.host != '127.0.0.1') {
      throw StateError('管理接口地址无效');
    }
    return ControlApi(endpoint, data['token'] as String);
  }

  Future<Map<String, dynamic>> request(String method, String path,
      [Map<String, dynamic>? data]) async {
    final request = await _client.openUrl(method, endpoint.resolve(path));
    request.headers.set(HttpHeaders.authorizationHeader, 'Bearer $token');
    if (data != null) {
      request.headers.contentType = ContentType.json;
      request.write(jsonEncode(data));
    }
    final response = await request.close().timeout(const Duration(seconds: 150));
    final text = await utf8.decoder.bind(response).join();
    if (response.statusCode != 200) {
      throw HttpException(text, uri: endpoint.resolve(path));
    }
    return jsonDecode(text) as Map<String, dynamic>;
  }

  void close() => _client.close(force: true);
}
