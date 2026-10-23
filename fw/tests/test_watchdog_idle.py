"""Compile the actual 0055 watchdog and virtio snapshot functions with small stubs.

Requires PVISOR_TEST_WATCHDOG_TREE pointing at a maintained, uninstrumented
kernel tree. This verifies guard/restoration contracts, not SMP concurrency.
"""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


class WatchdogIdleTests(unittest.TestCase):
    def test_actual_guard_and_timer_ownership(self):
        root = os.getenv('PVISOR_TEST_WATCHDOG_TREE')
        if not root:
            self.skipTest('PVISOR_TEST_WATCHDOG_TREE required')
        source = (Path(root) / 'net/sched/sch_generic.c').read_text()
        code = source[source.index('enum pvisor_watchdog_reason'):source.index('static void dev_watchdog(')]
        driver = (Path(root) / 'drivers/net/virtio_net.c').read_text()
        start = driver.index('static int virtnet_pvisor_tx_pending(')
        driver = driver[start:driver.index('\n}', start)+2]
        harness = r'''
#include <assert.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <string.h>
#define EOPNOTSUPP 95
#define EIO 5
#define READ_ONCE(x) (x)
#define BIT(n) (1UL << (n))
#define QUEUE_STATE_ANY_XOFF_OR_FROZEN 7
#define __QDISC_STATE_SCHED 0
#define QDISC_STATE_MISSED 4
#define QDISC_STATE_DRAINING 8
struct Qdisc { bool empty, running; struct { int qlen; } gso_skb, skb_bad_txq; unsigned long state; };
struct netdev_queue { unsigned long state; struct Qdisc *qdisc; };
struct timer_list { bool pending; unsigned long expires; };
struct virtqueue { unsigned int num_free, size; bool broken; };
struct virtnet_info { unsigned int max_queue_pairs; struct { struct virtqueue *vq; } sq[2]; };
struct net_device;
struct net_device_ops { int (*ndo_pvisor_tx_pending)(struct net_device *, unsigned int); };
struct net_device {
 struct net_device_ops *netdev_ops;
 bool present, running, carrier, noop, pvisor_watchdog_parked;
 unsigned long pvisor_watchdog_expires, watchdog_timeo;
 struct timer_list watchdog_timer;
 int tx_global_lock, pending[2]; unsigned int num_tx_queues;
 struct netdev_queue queues[2]; struct Qdisc q[2]; struct virtnet_info vi;
};
static struct net_device dev;
static struct net_device *fixture_ptr=&dev;
static unsigned long jiffies;
static unsigned int cpus=1;
static bool pvisor_watchdog_defer, irq_off=true, lock_ok=true, cancel_ok=true;
static int deletes, mods;
#define num_possible_cpus() (cpus)
#define lockdep_assert_irqs_disabled() assert(irq_off)
#define rcu_read_lock() ((void)0)
#define rcu_read_unlock() ((void)0)
#define for_each_netdev_rcu(net, d) for ((d)=fixture_ptr; (d); (d)=NULL)
#define spin_trylock(lock) (lock_ok)
#define spin_unlock(lock) ((void)0)
#define rcu_dereference(x) (x)
#define netif_device_present(d) ((d)->present)
#define netif_running(d) ((d)->running)
#define netif_carrier_ok(d) ((d)->carrier)
#define qdisc_tx_is_noop(d) ((d)->noop)
#define timer_pending(t) ((t)->pending)
#define time_after(a,b) ((long)((b)-(a)) < 0)
#define qdisc_is_empty(q) ((q)->empty)
#define qdisc_is_running(q) ((q)->running)
#define netdev_get_tx_queue(d,i) (&(d)->queues[i])
#define pvisor_watchdog_trace(...) ((void)0)
#define netdev_priv(d) (&(d)->vi)
#define virtqueue_is_broken(q) ((q)->broken)
#define virtqueue_get_vring_size(q) ((q)->size)
static int timer_delete(struct timer_list *t) {
 deletes++; if (!cancel_ok) return 0;
 int had=t->pending;t->pending=false;return had;
}
static int mod_timer(struct timer_list *t, unsigned long expires) {
 mods++;assert(!t->pending);t->pending=true;t->expires=expires;return 0;
}
static int snapshot(struct net_device *d,unsigned int i) { return d->pending[i]; }
static struct net_device_ops ops={snapshot};
static void reset(void) {
 memset(&dev,0,sizeof(dev));cpus=1;pvisor_watchdog_defer=true;
 lock_ok=cancel_ok=true;deletes=mods=0;jiffies=100;
 dev.netdev_ops=&ops;dev.present=dev.running=dev.carrier=true;
 dev.num_tx_queues=2;dev.watchdog_timer.pending=true;dev.watchdog_timer.expires=200;
 for(int i=0;i<2;i++){dev.queues[i].qdisc=&dev.q[i];dev.q[i].empty=true;}
}
'''
        cases = r'''
static void unchanged(void) {
 pvisor_net_idle_enter(); assert(!dev.pvisor_watchdog_parked);
 assert(dev.watchdog_timer.pending && dev.watchdog_timer.expires==200);
 pvisor_net_idle_exit();assert(mods==0);
}
int main(void) {
 reset();pvisor_net_idle_enter();assert(dev.pvisor_watchdog_parked && deletes==1);
 assert(!dev.watchdog_timer.pending);jiffies=900;
 pvisor_net_idle_exit();assert(dev.watchdog_timer.expires==200 && mods==1);
 assert(!dev.pvisor_watchdog_parked);pvisor_net_idle_exit();assert(mods==1);
 for(int state=1;state<8;state++) { reset();dev.queues[1].state=state;unchanged();assert(deletes==0); }
 for(int n=-3;n<5;n++) {if(!n)continue;reset();dev.pending[1]=n;unchanged();assert(deletes==0);}
 reset();dev.q[1].empty=false;unchanged();
 reset();dev.q[0].running=true;unchanged();
 reset();dev.q[0].gso_skb.qlen=1;unchanged();
 reset();dev.q[1].skb_bad_txq.qlen=1;unchanged();
 for(int n=1;n<=8;n*=2){reset();if(n==2)continue;dev.q[1].state=n;unchanged();}
 reset();pvisor_watchdog_defer=false;unchanged();assert(deletes==0);
 reset();cpus=2;unchanged();assert(deletes==0);
 reset();lock_ok=false;unchanged();assert(deletes==0);
 reset();cancel_ok=false;unchanged();assert(deletes==1);
 reset();dev.present=false;unchanged();reset();dev.running=false;unchanged();
 reset();dev.carrier=false;unchanged();reset();dev.noop=true;unchanged();
 reset();jiffies=200;unchanged();reset();jiffies=201;unchanged();
 reset();dev.watchdog_timer.pending=false;pvisor_net_idle_enter();pvisor_net_idle_exit();assert(!deletes&&!mods);
 reset();dev.netdev_ops=NULL;dev.netdev_ops=&ops;ops.ndo_pvisor_tx_pending=NULL;unchanged();ops.ndo_pvisor_tx_pending=snapshot;
 /* Rejected/short handoff restores before any IRQ/task can run. */
 reset();pvisor_net_idle_enter();pvisor_net_idle_exit();assert(mods==1 && dev.watchdog_timer.expires==200);
 /* jiffies wrap: preserve the original absolute expiration. */
 reset();jiffies=~0UL-10;dev.watchdog_timer.expires=20;
 pvisor_net_idle_enter();assert(dev.pvisor_watchdog_parked);jiffies=5;
 pvisor_net_idle_exit();assert(dev.watchdog_timer.expires==20);
 /* A subsequent busy generation cannot inherit empty eligibility. */
 reset();pvisor_net_idle_enter();pvisor_net_idle_exit();dev.pending[0]=1;
 pvisor_net_idle_enter();assert(!dev.pvisor_watchdog_parked && deletes==1);
 struct virtqueue v={.size=256,.num_free=256};
 reset();dev.vi.max_queue_pairs=1;dev.vi.sq[0].vq=&v;
 assert(virtnet_pvisor_tx_pending(&dev,0)==0);v.num_free=252;
 assert(virtnet_pvisor_tx_pending(&dev,0)==4);v.num_free=257;
 assert(virtnet_pvisor_tx_pending(&dev,0)<0);v.num_free=256;v.broken=true;
 assert(virtnet_pvisor_tx_pending(&dev,0)<0);v.broken=false;
 assert(virtnet_pvisor_tx_pending(&dev,1)<0);cpus=2;
 assert(virtnet_pvisor_tx_pending(&dev,0)<0);cpus=1;dev.vi.sq[0].vq=NULL;
 assert(virtnet_pvisor_tx_pending(&dev,0)<0);
 return 0;
}
'''
        with tempfile.TemporaryDirectory() as tmp:
            p = Path(tmp)/'watchdog.c'
            p.write_text(harness+code+driver+cases)
            subprocess.run([os.environ.get('CC','cc'),'-std=gnu11','-Wall','-Werror','-O2',str(p),'-o',str(p.with_suffix(''))],check=True)
            subprocess.run([str(p.with_suffix(''))],check=True)
