import 'dart:convert';
import 'dart:io';
import 'package:flutter/material.dart';
import 'package:desktop_commander/api.dart';
import 'package:desktop_commander/main.dart';

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  final timestamp = Platform.environment['DC_E2E_TIMESTAMP'];
  final expected = Platform.environment['DC_E2E_EXPECTED_TIME'];
  final output = Platform.environment['DC_E2E_OUTPUT'];
  if (timestamp == null || expected == null || output == null) {
    throw StateError('Live call timestamp, expected time and output path are required');
  }
  final api = await ControlApi.load();
  final before = await api.request('GET', '/status');
  final event = (before['events'] as List).cast<Map<String, dynamic>>()
      .singleWhere((item) => item['timestamp'] == timestamp);
  if ((before['status'] as Map)['state'] != 'online') {
    throw StateError('The actual Remote agent must be online');
  }
  runApp(const CommanderApp());
  final texts = <String>{};
  ScrollableState? scrollable;
  void collect(Element element) {
    final widget = element.widget;
    if (widget is Text && widget.data != null) texts.add(widget.data!);
    if (element is StatefulElement && element.state is ScrollableState) {
      final state = element.state as ScrollableState;
      if (state.position.axis == Axis.vertical) scrollable = state;
    }
    element.visitChildElements(collect);
  }
  final deadline = DateTime.now().add(const Duration(seconds: 30));
  while (DateTime.now().isBefore(deadline)) {
    await Future<void>.delayed(const Duration(milliseconds: 200));
    texts.clear();
    final root = WidgetsBinding.instance.rootElement;
    if (root != null) collect(root);
    if (texts.contains(expected) && texts.contains(event['toolName'])) break;
    final position = scrollable?.position;
    if (position != null && position.hasContentDimensions) {
      position.jumpTo(position.maxScrollExtent);
    }
  }
  if (!texts.contains(expected) || !texts.contains(event['toolName'])) {
    throw StateError('Live call was not rendered with the expected local time: $expected');
  }
  final after = await api.request('GET', '/status');
  final current = (after['events'] as List).cast<Map<String, dynamic>>()
      .singleWhere((item) => item['timestamp'] == timestamp);
  if (current['timestamp'] != event['timestamp'] || before['pid'] != after['pid']) {
    throw StateError('The source UTC event and actual backend must remain unchanged');
  }
  await File(output).writeAsString(jsonEncode({
    'passed': true, 'backendPid': before['pid'], 'backendOnline': true,
    'toolName': event['toolName'], 'sourceTimestamp': timestamp,
    'displayedText': expected, 'processTimezone': Platform.environment['TZ'],
    'sourceTimestampUnchanged': true,
  }));
  api.close();
  exit(0);
}
