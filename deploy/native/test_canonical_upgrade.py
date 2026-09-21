#!/usr/bin/env python3
"""Verify paused-writer ordering and restoration around the identity cutover."""
from unittest.mock import patch
import unittest
import canonical_upgrade as cutover


class CutoverTests(unittest.TestCase):
    def test_writers_stop_before_backup_and_import_finishes_before_start(self):
        events = []
        def callback(name):
            return lambda: events.append(name)
        with patch.object(cutover.upgrade, 'run', side_effect=lambda *args: events.append(' '.join(args))):
            cutover.pause_apply(*(callback(name) for name in ['backup', 'migrate', 'activate', 'verify', 'restore']))
        self.assertEqual(events, ['systemctl stop waveform-api', 'backup', 'migrate', 'activate',
                                  'systemctl start waveform-api', 'verify'])

    def test_backup_or_import_or_new_readiness_failure_restores_old_service(self):
        for failed in ['backup', 'migrate', 'activate', 'verify']:
            with self.subTest(failed=failed):
                events = []
                triggered = False
                def callback(name):
                    def call():
                        nonlocal triggered
                        events.append(name)
                        if name == failed and not triggered:
                            triggered = True
                            raise RuntimeError('fixture failure')
                    return call
                with patch.object(cutover.upgrade, 'run', side_effect=lambda *args: events.append(' '.join(args))), \
                     patch.object(cutover.subprocess, 'run', side_effect=lambda args, **kwargs: events.append(' '.join(args))):
                    with self.assertRaisesRegex(RuntimeError, 'fixture failure'):
                        cutover.pause_apply(*(callback(name) for name in ['backup', 'migrate', 'activate', 'verify', 'restore']))
                self.assertEqual(events[-5:], ['systemctl stop waveform-api', 'restore',
                    'systemctl reset-failed waveform-api', 'systemctl start waveform-api', 'verify'])
                if failed in ['backup', 'migrate']:
                    self.assertNotIn('activate', events)


if __name__ == '__main__':
    unittest.main()
