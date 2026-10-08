import pytest
import json
import sys
from pathlib import Path
import filesystem_stage_durability
from reference_baselines import digest
from filesystem_stage_durability import validate_completion


@pytest.mark.parametrize("fault", [None, "missing-seal", "wrong-policy", "invalid-seal"])
def test_measurements_require_the_requested_policy_and_completed_persistence(tmp_path, fault):
    journal = tmp_path / "preimages"
    journal.mkdir()
    (journal / "durability-v1").write_bytes(b"pvisor.stage.checkpoint/1\n")
    (journal / "sealed-v1").write_bytes(b"pvisor.stage.sealed/1\n")
    if fault == "missing-seal":
        (journal / "sealed-v1").unlink()
    elif fault == "wrong-policy":
        (journal / "durability-v1").write_bytes(b"pvisor.stage.strict/1\n")
    elif fault == "invalid-seal":
        (journal / "sealed-v1").write_bytes(b"partial")
    if fault:
        with pytest.raises((ValueError, FileNotFoundError)):
            validate_completion(tmp_path, "checkpoint")
    else:
        validate_completion(tmp_path, "checkpoint")


@pytest.mark.parametrize('fault',['preflight','warmup','final-input'])
def test_failed_durability_cohort_keeps_records_and_final_audit(tmp_path,monkeypatch,fault):
    assets=tmp_path/'assets';assets.mkdir();(assets/'assets.json').write_text('{}')
    product=tmp_path/'product';product.mkdir();binary=product/'pvisor';binary.write_bytes(b'frozen CLI')
    receipt=product/'build-receipt.json';receipt.write_text('{}');(product/'source-manifest.json').write_text('[]')
    firmware=tmp_path/'firmware';firmware.mkdir();(firmware/'libkrunfw.so.5').write_bytes(b'kernel')
    output=tmp_path/'.data/cohort';checks=[];trials=[]
    def verify_inputs(_):
        checks.append(1)
        if fault=='final-input' and len(checks)==2:raise ValueError('changed input')
        return dict(files=1)
    def run(args,metadata,backend,mode,trial):
        cell=args.stage_durability or 'native';trials.append((cell,trial))
        if cell=='strict' and ((fault=='preflight' and trial==-100) or (fault=='warmup' and trial==-1)):
            raise ValueError('failed actual condition')
        root=args.output/'trials'/str(trial);journal=root/'stage/preimages';journal.mkdir(parents=True)
        if cell!='native':
            (journal/'durability-v1').write_bytes(f'pvisor.stage.{cell}/1\n'.encode())
            (journal/'sealed-v1').write_bytes(b'pvisor.stage.sealed/1\n')
        return dict(backend=backend,trial=trial,logs=str(root),completion_ms=10,
                    result={'filesystem':{op:{'worker_ms':2} for op in filesystem_stage_durability.WORKLOADS}})
    monkeypatch.setattr(filesystem_stage_durability,'verified_build_receipt',lambda *a:dict(pvisor_sha256=digest(binary)))
    monkeypatch.setattr(filesystem_stage_durability,'verify_reference_inputs',verify_inputs)
    monkeypatch.setattr(filesystem_stage_durability,'run_trial',run)
    monkeypatch.setattr(sys,'argv',['durability','--assets',str(assets),'--binary',str(binary),'--build-receipt',str(receipt),
                                  '--firmware',str(firmware),'--output',str(output),'--samples','1','--warmups','1'])
    with pytest.raises(SystemExit) as error:filesystem_stage_durability.main()
    assert error.value.code==1 and len(checks)==2
    report=json.loads((output/'report.json').read_text());assert report['state']=='failed'
    assert 'comparisons' not in report and 'summary' not in report
    if fault=='preflight':assert len(trials)==3 and report['samples']==[]
    else:assert len(trials)==9 and len(report['samples'])==3
    assert report['input_final_verification']['state']==('failed' if fault=='final-input' else 'passed')
