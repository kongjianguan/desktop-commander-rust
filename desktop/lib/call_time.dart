import 'package:intl/intl.dart';

final _callTimeFormat = DateFormat('yyyy-MM-dd HH:mm:ss.SSS', 'en_US');

String formatCallTime(String timestamp) {
  final local = DateTime.parse(timestamp).toLocal();
  final offset = local.timeZoneOffset;
  final minutes = offset.inMinutes.abs();
  final hoursText = (minutes ~/ 60).toString().padLeft(2, '0');
  final minutesText = (minutes % 60).toString().padLeft(2, '0');
  final sign = offset.isNegative ? '-' : '+';
  return '${_callTimeFormat.format(local)} UTC$sign$hoursText:$minutesText';
}
