(ns clojure.set)

(defn union
  ([] #{})
  ([s1] s1)
  ([s1 s2] (into (or s1 #{}) s2))
  ([s1 s2 & sets] (reduce union (union s1 s2) sets)))

(defn intersection
  ([s1] s1)
  ([s1 s2]
   (if (< (count s2) (count s1))
     (intersection s2 s1)
     (reduce (fn [result item]
               (if (contains? s2 item) result (disj result item)))
             s1 s1)))
  ([s1 s2 & sets] (reduce intersection (intersection s1 s2) sets)))

(defn difference
  ([s1] s1)
  ([s1 s2]
   (if (< (count s1) (count s2))
     (reduce (fn [result item]
               (if (contains? s2 item) (disj result item) result))
             s1 s1)
     (reduce disj s1 s2)))
  ([s1 s2 & sets] (reduce difference (difference s1 s2) sets)))

(defn select [pred xset]
  (reduce (fn [s k] (if (pred k) s (disj s k))) xset xset))

(defn project [xrel ks]
  (set (map #(select-keys % ks) xrel)))

(defn rename-keys [map kmap]
  (reduce (fn [m [old new]]
            (if (contains? m old)
              (assoc (dissoc m old) new (get m old))
              m))
          map kmap))

(defn rename [xrel kmap]
  (set (map #(rename-keys % kmap) xrel)))

(defn index [xrel ks]
  (reduce (fn [m x]
            (let [ik (select-keys x ks)]
              (assoc m ik (conj (get m ik #{}) x))))
          {} xrel))

(defn map-invert [m]
  (reduce (fn [m2 [k v]] (assoc m2 v k)) {} m))

(defn join
  ([xrel yrel]
   (if (and (seq xrel) (seq yrel))
     (let [ks (intersection (set (keys (first xrel)))
                            (set (keys (first yrel))))
           idx (index (if (<= (count xrel) (count yrel)) xrel yrel) ks)
           r   (if (<= (count xrel) (count yrel)) yrel xrel)]
       (reduce (fn [ret x]
                 (let [found (get idx (select-keys x ks))]
                   (if found
                     (reduce #(conj %1 (merge %2 x)) ret found)
                     ret)))
               #{} r))
     #{}))
  ([xrel yrel km]
   (let [idx (index yrel (vals km))]
     (reduce (fn [ret x]
               (let [found (get idx (rename-keys (select-keys x (keys km)) km))]
                 (if found
                   (reduce #(conj %1 (merge %2 x)) ret found)
                   ret)))
             #{} xrel))))

(defn subset? [set1 set2]
  (and (<= (count set1) (count set2))
       (every? #(contains? set2 %) set1)))

(defn superset? [set1 set2]
  (and (>= (count set1) (count set2))
       (every? #(contains? set1 %) set2)))
