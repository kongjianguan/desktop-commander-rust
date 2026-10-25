import 'dart:convert';
import 'dart:io';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';
import 'package:desktop_commander/main.dart';

void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  testWidgets('管理界面操作真实 Rust 后台并保存设置', (tester) async {
    final bundle = Platform.environment['DC_BUNDLE'];
    if (bundle == null) throw StateError('DC_BUNDLE is required');
    final work = Directory('${Directory.current.path}/../.work/flutter-e2e');
    await work.create(recursive: true);
    await File('${work.path}/settings.json').writeAsString(jsonEncode({'autoConnect': false}));
    final executable = '$bundle/desktop-commander${Platform.isWindows ? '.exe' : ''}';
    final child = await Process.start(executable, ['--config-dir', work.path, 'remote', '--headless']);
    final errors = File('${work.path}/stderr.log').openWrite();
    child.stderr.listen(errors.add);
    final descriptor = File('${work.path}/control.json');
    try {
      for (var i = 0; i < 100 && !await descriptor.exists(); i++) {
        await Future<void>.delayed(const Duration(milliseconds: 100));
      }
      final data = jsonDecode(await descriptor.readAsString()) as Map<String, dynamic>;
      // Flutter runner 的进程环境由测试驱动入口继承。
      final api = HttpClient()..findProxy = (_) => 'DIRECT';
      final request = await api.getUrl(Uri.parse('${data['endpoint']}/status'));
      request.headers.set('Authorization', 'Bearer ${data['token']}');
      final response = await request.close();
      expect(response.statusCode, 200);
      api.close();
      await tester.pumpWidget(CommanderApp(descriptorPath: descriptor.path));
      for (var i = 0; i < 30; i++) {
        await tester.pump(const Duration(milliseconds: 200));
        if (find.byKey(const Key('device-name')).evaluate().isNotEmpty) break;
      }
      expect(find.text('远程连接已停止'), findsOneWidget);
      await tester.enterText(find.byKey(const Key('device-name')), 'Flutter E2E 设备');
      await tester.ensureVisible(find.byKey(const Key('save-settings')));
      await tester.tap(find.byKey(const Key('save-settings')));
      for (var i = 0; i < 30; i++) {
        await tester.pump(const Duration(milliseconds: 200));
        final saved = jsonDecode(await File('${work.path}/settings.json').readAsString()) as Map;
        if (saved['deviceName'] == 'Flutter E2E 设备') break;
      }
      final saved = jsonDecode(await File('${work.path}/settings.json').readAsString()) as Map;
      expect(saved['deviceName'], 'Flutter E2E 设备');
      await tester.pumpWidget(const SizedBox.shrink());
      expect(await descriptor.exists(), isTrue);
      final quitApi = HttpClient()..findProxy = (_) => 'DIRECT';
      final quit = await quitApi.postUrl(Uri.parse('${data['endpoint']}/quit'));
      quit.headers.set('Authorization', 'Bearer ${data['token']}');
      await (await quit.close()).drain<void>();
      quitApi.close();
      expect(await child.exitCode, 0);
      await File('${work.path}/result.json').writeAsString(jsonEncode({'savedSettings':true,'backendSurvivesWindowDisposal':true}));
    } finally {
      child.kill();
      await errors.close();
    }
  });
}
