#include "mino.h"
#include <stdio.h>
int main(void){
  mino_state *S=mino_state_new(); mino_env *e=mino_env_new(S);
  mino_install_all(S,e);
  mino_val *r=mino_eval_string(S,"(+ 1 2)",e);
  long long n=0; if(!r||!mino_to_int(r,&n)){fprintf(stderr,"eval failed: %s\n",mino_last_error(S));return 1;}
  printf("(+ 1 2) = %lld\n",n);
  r=mino_eval_string(S,"(do (require (quote mino.store)) (def c (mino.store/open)) (mino.store/transact c {:alice {:age 30}}) (mino.store/read (mino.store/db c) :alice :age))",e);
  if(!r||!mino_to_int(r,&n)){fprintf(stderr,"store failed: %s\n",mino_last_error(S));return 1;}
  printf("store :alice :age = %lld\n",n);
  mino_env_free(S,e); mino_state_free(S); puts("SMOKE_OK"); return 0;
}
