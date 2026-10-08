import hashlib
import pytest
from v1.oci import verify_prepared


def test_pinned_prepared_rootfs_rejects_content_and_inventory_changes(tmp_path):
    file=tmp_path/'file';file.write_text('fixed input');link=tmp_path/'link';link.symlink_to('file')
    manifest=dict(rootfs_manifest=[dict(path='file',kind='file',mode=file.lstat().st_mode,sha256=hashlib.sha256(file.read_bytes()).hexdigest()),
                                  dict(path='link',kind='symlink',mode=link.lstat().st_mode,target='file')])
    verify_prepared(tmp_path,manifest)
    file.write_text('different bytes')
    with pytest.raises(ValueError,match='contents'):verify_prepared(tmp_path,manifest)
    file.write_text('fixed input');(tmp_path/'unexpected').touch()
    with pytest.raises(ValueError,match='inventory'):verify_prepared(tmp_path,manifest)
