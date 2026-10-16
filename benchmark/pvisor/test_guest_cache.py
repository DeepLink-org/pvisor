"""Trust-boundary checks for the guest-cache experiment, without hardware."""
import json
import unittest
import guest_cache as bench

class GuestCacheTests(unittest.TestCase):
    def payload(self, replace=None):
        rows=[{'phase':p,'ms':1.0} for p in bench.PHASES]
        if replace is not None:rows[0]['ms']=replace
        return ('\n'.join('CACHE_RESULT '+json.dumps(row) for row in rows)+'\nCACHE_PASS all\n').encode()

    def test_complete_finite_phases_only(self):
        self.assertEqual(set(bench.results(self.payload(),'all')),set(bench.PHASES))
        for invalid in [True,-1,float('nan'),float('inf')]:
            with self.assertRaises(ValueError):bench.results(self.payload(invalid),'all')
        for invalid in [b'CACHE_PASS all\n',self.payload()+b'CACHE_PASS all\n',self.payload().replace(b'stat_hot',b'stat_cold')]:
            with self.assertRaises(ValueError):bench.results(invalid,'all')

    def test_profile_mode_cannot_supply_timing_rows(self):
        self.assertEqual(bench.results(b'CACHE_PASS open\n','open'),{})
        with self.assertRaises(ValueError):bench.results(self.payload(),'open')

    def test_incomplete_pair_is_rejected(self):
        with self.assertRaises(ValueError):
            bench.analyze({'cohorts':['sdk'],'samples':1,'rows':[{'round':0,'cohort':'sdk','arm':0,'times':dict.fromkeys(bench.PHASES,1)}]})

if __name__=='__main__':unittest.main()
