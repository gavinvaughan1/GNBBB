// pgb FILE [NT] : per-page cost of mapping / releasing a (tmpfs) file, cold vs warm struct pages.
// Prints ns per 4 kB page: f=fault-touch, p=MADV_POPULATE_READ, d=MADV_DONTNEED; c=cold (after a 256 MB
// cache sweep), w=warm (struct pages touched just before); suffix 1 = one thread, N = NT threads.
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <fcntl.h>
#include <unistd.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <pthread.h>
#include <time.h>
#include <stdint.h>
#include <ftw.h>
static char bestp[4096]; static off_t bests=-1;
static int visit(const char*p,const struct stat*s,int t,struct FTW*f){(void)f; if(t==FTW_F&&S_ISREG(s->st_mode)&&s->st_size>bests){bests=s->st_size;strncpy(bestp,p,4095);} return 0;}
#ifndef MADV_POPULATE_READ
#define MADV_POPULATE_READ 22
#endif
static double now(void){struct timespec t;clock_gettime(CLOCK_MONOTONIC,&t);return t.tv_sec*1e9+t.tv_nsec;}
static char *sweep; static size_t SW=256u<<20;
static void evict(void){ volatile uint64_t x=0; for(size_t i=0;i<SW;i+=64) sweep[i]++, x+=sweep[i]; }
static char *base; static size_t len; static int nt;
typedef struct{int id; int op; double ns;} J;
static volatile uint64_t sink;
static void *work(void *a){ J*j=a; size_t per=(len/nt)&~((size_t)(2<<20)-1); char*s=base+per*j->id; size_t n=(j->id==nt-1)? len-per*j->id : per;
  double t=now(); uint64_t x=0;
  if(j->op==0){ for(size_t i=0;i<n;i+=4096) x+=s[i]; }
  else if(j->op==1){ madvise(s,n,MADV_POPULATE_READ); }
  else { madvise(s,n,MADV_DONTNEED); }
  j->ns=now()-t; sink+=x; return 0; }
static double run(int op,int threads){ nt=threads; pthread_t th[64]; J j[64]; double t=now();
  for(int i=0;i<nt;i++){j[i].id=i;j[i].op=op;pthread_create(&th[i],0,work,&j[i]);} for(int i=0;i<nt;i++) pthread_join(th[i],0);
  double w=now()-t; return w; }
int main(int c,char**v){ struct stat s0; const char*path=v[1]; if(stat(path,&s0)==0&&S_ISDIR(s0.st_mode)){ nftw(path,visit,16,FTW_PHYS); path=bestp; } int fd=open(path,O_RDONLY); if(fd<0){printf("pgb !open\n");return 1;} struct stat st; fstat(fd,&st); len=st.st_size&~4095UL; size_t lim=c>3? (size_t)atol(v[3])<<20:0; if(lim&&len>lim)len=lim;
  int N=c>2?atoi(v[2]):8; size_t np=len/4096;
  base=mmap(0,len,PROT_READ,MAP_SHARED,fd,0); sweep=mmap(0,SW,PROT_READ|PROT_WRITE,MAP_PRIVATE|MAP_ANONYMOUS|MAP_POPULATE,-1,0);
  double r[24]; int k=0; const char* nm[24];
  for(int T=1;T<=N;T+=N-1){ if(T>N)break;
    // cold fault, warm release
    evict(); r[k]=run(0,T)/np*T; nm[k++]= T==1?"f1c":"fNc"; r[k]=run(2,T)/np*T; nm[k++]= T==1?"d1w":"dNw";
    // warm fault (struct pages just touched by the release), cold release
    r[k]=run(0,T)/np*T; nm[k++]= T==1?"f1w":"fNw"; evict(); r[k]=run(2,T)/np*T; nm[k++]= T==1?"d1c":"dNc";
    // cold populate, then the cold data miss alone (pages mapped), then release
    evict(); r[k]=run(1,T)/np*T; nm[k++]= T==1?"p1c":"pNc"; evict(); r[k]=run(0,T)/np*T; nm[k++]= T==1?"m1c":"mNc"; run(2,T);
    if(N==1)break; }
  printf("pgb np=%zu", np); for(int i=0;i<k;i++) printf(" %s=%.0f", nm[i], r[i]); printf("\n"); return 0; }
