import pytest
from vm_memory import validate_restore


def test_restore_requires_original_execution_and_all_memory_bytes():
    before=dict(kind='random',bytes=64*1024**2,token='unique-before-suspend',checksum='0123456789abcdef')
    validate_restore(before,before|dict(integrity='passed'))
    for changed in (dict(token='fresh-process'),dict(checksum='wrong'),dict(bytes=0),dict(kind='repeated'),dict(integrity='failed')):
        with pytest.raises(ValueError,match='complete guest memory'):
            validate_restore(before,before|dict(integrity='passed')|changed)
