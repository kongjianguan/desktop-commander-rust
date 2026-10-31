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
  Element? matchedRow;
  ScrollableState? scrollable;
  void collect(Element element) {
    final widget = element.widget;
    if (widget is ListTile && widget.title is Text && widget.subtitle is Text &&
        (widget.title as Text).data == event['toolName'] &&
        (widget.subtitle as Text).data == expected) {
      matchedRow = element;
    }
    if (element is StatefulElement && element.state is ScrollableState) {
      final state = element.state as ScrollableState;
      if (state.position.axis == Axis.vertical) scrollable = state;
    }
    element.visitChildElements(collect);
  }
  final deadline = DateTime.now().add(const Duration(seconds: 30));
  while (DateTime.now().isBefore(deadline)) {
    await Future<void>.delayed(const Duration(milliseconds: 200));
    matchedRow = null;
    final root = WidgetsBinding.instance.rootElement;
    if (root != null) collect(root);
    if (matchedRow != null) break;
    final position = scrollable?.position;
    if (position != null && position.hasContentDimensions) {
      position.jumpTo(position.maxScrollExtent);
    }
  }
  final row = matchedRow;
  if (row == null) {
    throw StateError('Live call was not rendered with the expected local time: $expected');
  }
  await Scrollable.ensureVisible(row, alignment: 0.5,
      duration: const Duration(milliseconds: 200));
  await WidgetsBinding.instance.endOfFrame;
  final box = row.findRenderObject();
  if (box is! RenderBox || !box.hasSize) {
    throw StateError('Live call row has no rendered layout');
  }
  final view = View.of(row);
  final viewport = Offset.zero & (view.physicalSize / view.devicePixelRatio);
  if (!viewport.contains(box.localToGlobal(box.size.center(Offset.zero)))) {
    throw StateError('Live call row did not scroll into the visible viewport');
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
    'sourceTimestampUnchanged': true, 'matchingRowVisible': true,
  }));
  api.close();
  exit(0);
}
