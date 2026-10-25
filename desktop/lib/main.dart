import 'dart:async';
import 'dart:io';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'api.dart';

void main() => runApp(const CommanderApp());

class CommanderApp extends StatelessWidget {
  final String? descriptorPath;
  const CommanderApp({super.key, this.descriptorPath});
  @override
  Widget build(BuildContext context) => MaterialApp(
    title: 'Desktop Commander',
    debugShowCheckedModeBanner: false,
    theme: ThemeData(
      colorScheme: ColorScheme.fromSeed(seedColor: const Color(0xFF6366F1)),
      scaffoldBackgroundColor: const Color(0xFFF7F8FC),
      useMaterial3: true,
      inputDecorationTheme: const InputDecorationTheme(
        border: OutlineInputBorder(), isDense: true,
        filled: true, fillColor: Colors.white,
      ),
    ),
    home: ControlPanel(descriptorPath: descriptorPath),
  );
}

class ControlPanel extends StatefulWidget {
  final String? descriptorPath;
  const ControlPanel({super.key, this.descriptorPath});
  @override
  State<ControlPanel> createState() => _ControlPanelState();
}

class _ControlPanelState extends State<ControlPanel> {
  ControlApi? _api;
  Timer? _timer;
  Map<String, dynamic>? _snapshot;
  String? _error;
  String? _notice;
  bool _busy = false;
  bool _polling = false;
  bool _editing = false;
  final _name = TextEditingController();
  final _proxy = TextEditingController();
  final _noProxy = TextEditingController();
  bool _autoConnect = true;
  bool _preventSleep = false;
  bool _startAtLogin = false;

  @override
  void initState() {
    super.initState();
    _initialize();
  }

  Future<void> _initialize() async {
    try {
      _api = await ControlApi.load(descriptorPath: widget.descriptorPath);
      await _refresh();
      _timer = Timer.periodic(const Duration(seconds: 2), (_) => _refresh());
    } catch (error) {
      if (mounted) setState(() => _error = '$error');
    }
  }

  Future<void> _refresh() async {
    if (_polling || _api == null) return;
    _polling = true;
    try {
      final data = await _api!.request('GET', '/status');
      if (!mounted) return;
      if (_snapshot == null || !_editing) {
        final settings = data['settings'] as Map<String, dynamic>;
        _name.text = settings['deviceName'] as String;
        _proxy.text = settings['proxy'] as String;
        _noProxy.text = settings['noProxy'] as String;
        _autoConnect = settings['autoConnect'] as bool;
        _preventSleep = settings['preventSleep'] as bool;
        _startAtLogin = settings['startAtLogin'] as bool;
      }
      setState(() { _snapshot = data; _error = null; });
    } catch (error) {
      if (mounted) setState(() => _error = '$error');
    } finally {
      _polling = false;
    }
  }

  Future<void> _action(String path) async {
    if (_busy || _api == null) return;
    setState(() { _busy = true; _notice = null; });
    try {
      final response = await _api!.request('POST', path);
      if (mounted && response['path'] != null) {
        setState(() => _notice = '诊断文件已保存：${response['path']}');
      }
      await _refresh();
    } catch (error) {
      if (mounted) setState(() => _error = '$error');
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  Future<void> _save() async {
    final settings = Map<String, dynamic>.from(_snapshot!['settings'] as Map);
    settings.addAll({'deviceName': _name.text.trim(), 'proxy': _proxy.text.trim(),
      'noProxy': _noProxy.text.trim(), 'autoConnect': _autoConnect,
      'preventSleep': _preventSleep, 'startAtLogin': _startAtLogin});
    setState(() => _busy = true);
    try {
      await _api!.request('PUT', '/settings', settings);
      _editing = false;
      await _refresh();
      if (mounted) setState(() => _notice = '设置已保存');
    } catch (error) {
      if (mounted) setState(() => _error = '$error');
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  @override
  void dispose() {
    _timer?.cancel(); _api?.close();
    _name.dispose(); _proxy.dispose(); _noProxy.dispose();
    super.dispose();
  }

  Widget _card(Widget child) => Card(
    elevation: 0, color: Colors.white, margin: const EdgeInsets.only(bottom: 16),
    shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(16),
      side: const BorderSide(color: Color(0xFFE5E7F0))),
    child: Padding(padding: const EdgeInsets.all(22), child: child),
  );

  @override
  Widget build(BuildContext context) {
    final status = _snapshot?['status'] as Map<String, dynamic>?;
    final state = status?['state'] as String?;
    final online = state == 'online';
    final labels = {'online': '远程连接正常', 'connecting': '正在连接',
      'authorizing': '等待设备授权', 'reconnecting': '正在重新连接',
      'error': '连接发生错误', 'stopped': '远程连接已停止'};
    final events = (_snapshot?['events'] as List? ?? []).reversed.take(30).toList();
    return Scaffold(
      appBar: AppBar(title: const Text('Desktop Commander'),
        backgroundColor: Colors.transparent,
        actions: [Padding(padding: const EdgeInsets.only(right: 24),
          child: Center(child: Text('版本 ${_snapshot?['version'] ?? '—'}')))]),
      body: Align(alignment: Alignment.topCenter,
        child: ConstrainedBox(constraints: const BoxConstraints(maxWidth: 900),
          child: ListView(padding: const EdgeInsets.all(24), children: [
            _card(Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
              Row(children: [Icon(online ? Icons.cloud_done_outlined : Icons.cloud_off_outlined,
                size: 32, color: online ? const Color(0xFF047857) : const Color(0xFF6366F1)),
                const SizedBox(width: 14), Expanded(child: Text(labels[state] ?? '正在读取状态',
                  key: const Key('connection-status'), style: Theme.of(context).textTheme.titleLarge)),
                if (_busy) const SizedBox(width: 22, height: 22, child: CircularProgressIndicator(strokeWidth: 2))]),
              const SizedBox(height: 12),
              SelectableText(status?['detail'] as String? ?? '正在连接本机管理接口'),
              if ((status?['deviceId'] as String? ?? '').isNotEmpty) ...[
                const SizedBox(height: 10), SelectableText('设备编号：${status!['deviceId']}')],
              const SizedBox(height: 18),
              Wrap(spacing: 12, runSpacing: 12, children: [
                FilledButton.icon(key: const Key('connect'), onPressed: _busy || _api == null || online ? null : () => _action('/connect'),
                  icon: const Icon(Icons.play_arrow), label: const Text('连接远程服务')),
                OutlinedButton.icon(key: const Key('disconnect'), onPressed: _busy || _api == null ? null : () => _action('/disconnect'),
                  icon: const Icon(Icons.pause), label: const Text('停止远程连接')),
                TextButton(onPressed: _busy || _api == null ? null : () => _action('/export'), child: const Text('导出诊断文件')),
              ]),
              if ((status?['verificationUri'] as String? ?? '').isNotEmpty) ...[
                const Divider(height: 32), Text('请在浏览器确认验证码：${status!['userCode']}', style: Theme.of(context).textTheme.titleMedium),
                const SizedBox(height: 10), SelectableText(status['verificationUri'] as String),
                TextButton.icon(onPressed: () => Clipboard.setData(ClipboardData(text: status['verificationUri'] as String)),
                  icon: const Icon(Icons.copy), label: const Text('复制授权链接')),
              ],
            ])),
            if (_error != null) Padding(padding: const EdgeInsets.only(bottom: 16),
              child: Semantics(liveRegion: true, child: SelectableText(_error!, style: TextStyle(color: Theme.of(context).colorScheme.error)))),
            if (_notice != null) Padding(padding: const EdgeInsets.only(bottom: 16),
              child: Semantics(liveRegion: true, child: SelectableText(_notice!))),
            if (_snapshot != null) _card(Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
              Text('连接设置', style: Theme.of(context).textTheme.titleMedium),
              const SizedBox(height: 20),
              TextField(key: const Key('device-name'), controller: _name, onChanged: (_) => _editing = true,
                decoration: const InputDecoration(labelText: '设备名称')),
              const SizedBox(height: 16),
              TextField(controller: _proxy, onChanged: (_) => _editing = true,
                decoration: const InputDecoration(labelText: '代理地址', helperText: '留空时读取 HTTP_PROXY、HTTPS_PROXY 等环境变量')),
              const SizedBox(height: 16),
              TextField(controller: _noProxy, onChanged: (_) => _editing = true,
                decoration: const InputDecoration(labelText: '无需代理的地址')),
              const SizedBox(height: 12),
              SwitchListTile(contentPadding: EdgeInsets.zero, title: const Text('启动后连接远程服务'), value: _autoConnect,
                onChanged: _busy ? null : (value) => setState(() { _autoConnect = value; _editing = true; })),
              SwitchListTile(contentPadding: EdgeInsets.zero, title: const Text('登录系统后启动'), value: _startAtLogin,
                onChanged: _busy ? null : (value) => setState(() { _startAtLogin = value; _editing = true; })),
              SwitchListTile(contentPadding: EdgeInsets.zero, title: const Text('连接期间阻止系统睡眠'), value: _preventSleep,
                onChanged: _busy ? null : (value) => setState(() { _preventSleep = value; _editing = true; })),
              const SizedBox(height: 12),
              Wrap(spacing: 12, children: [FilledButton(key: const Key('save-settings'), onPressed: _busy ? null : _save, child: const Text('保存设置')),
                TextButton(onPressed: _busy ? null : () => _action('/logout'), child: const Text('退出当前账号'))]),
            ])),
            _card(Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
              Text('近期调用', style: Theme.of(context).textTheme.titleMedium), const SizedBox(height: 12),
              if (events.isEmpty) const Text('暂无工具调用'),
              ...events.map((dynamic value) { final event = value as Map;
                return ListTile(contentPadding: EdgeInsets.zero,
                  leading: Icon(event['success'] == true ? Icons.check_circle_outline : Icons.error_outline),
                  title: Text(event['toolName'] as String), subtitle: Text(event['timestamp'] as String),
                  trailing: Text('${event['durationMs']} ms')); }),
            ])),
            const Text('关闭管理窗口后，远程连接继续运行。通过状态栏菜单可以再次打开窗口。'),
            const SizedBox(height: 16),
            Align(alignment: Alignment.centerLeft, child: TextButton(
              onPressed: _busy || _api == null ? null : () async { await _action('/quit'); exit(0); },
              child: const Text('退出整个程序'))),
          ]))),
    );
  }
}
